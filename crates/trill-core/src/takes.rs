//! What a track records, and what becomes of it: takes, kept beside the
//! track's clips, and the comp that is made of them.
//!
//! A track that says nothing records what a keyboard plays, into notes. A
//! track with an [`TrackInput`] records an input of the audio device, and
//! every recording of it is a [`Take`]: a clip of its own that is kept in
//! a *lane* beside the track and never played from there. Nothing that
//! reads a track's clips sees a take; what the track plays is its clips,
//! as always.
//!
//! A *comp* is those clips. Putting a stretch of a take on the track
//! ([`Project::comp_take`]) makes an ordinary audio clip of that stretch,
//! in place of whatever the track had there, and marks the seams so that
//! the two sides are faded across one another where they meet
//! ([`AudioContent::crossfade`](crate::project::AudioContent)). Nothing
//! that was on the track is lost by it: a clip that is covered, by a comp
//! or by a new recording, is kept as a take first unless a take that plays
//! the same recording at the same place is there already
//! ([`Project::cover`]). So recording over a part, and choosing between
//! takes afterwards, never destroys anything, and each is one edit.

use crate::project::{AudioContent, ClipContent, EditError, TrackKind};
use crate::tempo::TempoMap;
use crate::{Beats, Clip, ClipId, Project, Track, TrackId};
use serde::{Deserialize, Serialize};

/// The most channels of an audio device's input a track can name.
pub const MAX_INPUT_CHANNEL: u16 = 32;
/// Two positions this close are the same one.
const TOUCHING: Beats = 1e-9;
/// The shortest stretch of a take that can be put on a track.
const SHORTEST: Beats = 1.0 / 1024.0;

/// When a track that records an input is heard through its own devices.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Monitoring {
    /// Never: the input is recorded and not heard through Trill. What an
    /// audio interface plays back of its own inputs is heard all the same.
    #[default]
    Off,
    /// While the track is armed.
    Armed,
    /// Always.
    Always,
}

impl Monitoring {
    fn is_off(&self) -> bool {
        *self == Monitoring::Off
    }
}

/// What a track records: an input of the audio device, which is the
/// machine's to choose, and how it is heard meanwhile.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrackInput {
    /// The channel of the device's input, counted from 1. Of a pair, the
    /// left one.
    pub channel: u16,
    /// Whether it is that channel and the one after it, as left and right.
    /// One channel alone is in the middle.
    #[serde(default, skip_serializing_if = "is_false")]
    pub stereo: bool,
    #[serde(default, skip_serializing_if = "Monitoring::is_off")]
    pub monitor: Monitoring,
}

fn is_false(flag: &bool) -> bool {
    !*flag
}

impl TrackInput {
    /// One channel of the input, not heard through Trill.
    pub fn mono(channel: u16) -> Self {
        Self { channel, stereo: false, monitor: Monitoring::Off }
    }

    /// The channels it takes, counted from 0: the left, and the right of a
    /// pair.
    pub fn channels(&self) -> (usize, Option<usize>) {
        let left = usize::from(self.channel.max(1)) - 1;
        (left, self.stereo.then_some(left + 1))
    }

    /// In words: "input 1", "inputs 1 and 2".
    pub fn said(&self) -> String {
        match self.stereo {
            true => format!("inputs {} and {}", self.channel, self.channel + 1),
            false => format!("input {}", self.channel),
        }
    }
}

/// A recording kept beside a track's clips: see the module documentation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Take {
    /// Which lane beside the track it is in, counted from 0.
    pub lane: u32,
    /// What it is: where it lies in the song, its name and its recording.
    /// Its id is from the same count as every clip's.
    pub clip: Clip,
}

impl Track {
    /// How many lanes of takes the track has.
    pub fn lanes(&self) -> u32 {
        self.takes.iter().map(|take| take.lane + 1).max().unwrap_or(0)
    }

    /// The take with this id, if it is one of the track's.
    pub fn take(&self, id: ClipId) -> Option<&Take> {
        self.takes.iter().find(|take| take.clip.id == id)
    }

    /// The first lane in which nothing lies over `start..end`.
    fn free_lane(&self, start: Beats, end: Beats) -> u32 {
        let taken = |lane: u32| {
            self.takes
                .iter()
                .any(|take| take.lane == lane && take.clip.start < end - TOUCHING && take.clip.end() > start + TOUCHING)
        };
        (0..).find(|lane| !taken(*lane)).unwrap_or(0)
    }
}

/// Whether two clips' audio is the same recording played the same way: all
/// but where in it they begin.
fn same_recording(a: &AudioContent, b: &AudioContent) -> bool {
    a.path == b.path
        && a.warp == b.warp
        && a.source_tempo == b.source_tempo
        && a.transpose == b.transpose
        && a.gain_db == b.gain_db
        && a.looped == b.looped
}

/// Whether `take` plays, over all of `clip`, exactly what `clip` plays
/// there: then nothing of the clip would be lost with it.
fn keeps(take: &Clip, clip: &Clip, map: &TempoMap) -> bool {
    let (Some(kept), Some(audio)) = (take.audio(), clip.audio()) else { return false };
    take.start <= clip.start + TOUCHING
        && take.end() >= clip.end() - TOUCHING
        && same_recording(kept, audio)
        && (kept.second_at(map, take.start, clip.start) - audio.wrapped(audio.offset_seconds)).abs() < 1e-6
}

impl Project {
    /// Says what a track records: an input of the audio device, or with
    /// `None` what a keyboard plays, as a track does that says nothing.
    /// Only a track that holds clips records, and not a reference.
    pub fn set_track_input(&mut self, id: TrackId, input: Option<TrackInput>) -> Result<(), EditError> {
        let track = self.track(id)?;
        if let Some(input) = input {
            match track.kind {
                TrackKind::Normal => {}
                TrackKind::Group => {
                    return invalid(format!("{id} ({}) is a group track and records nothing", track.name));
                }
                TrackKind::Return => {
                    return invalid(format!("{id} ({}) is a return track and records nothing", track.name));
                }
            }
            if track.reference.is_some() {
                return invalid(format!(
                    "{id} ({}) is a reference: a recording beside the song, which records nothing itself",
                    track.name
                ));
            }
            let last = input.channel.saturating_add(u16::from(input.stereo));
            if input.channel == 0 || last > MAX_INPUT_CHANNEL {
                return invalid(format!(
                    "an input is one of channels 1 to {MAX_INPUT_CHANNEL} of the audio device, or two of them side by side; {} is not",
                    input.said()
                ));
            }
        }
        self.track_mut(id)?.input = input;
        Ok(())
    }

    /// The take with this id and the track it is kept beside.
    pub fn take(&self, id: ClipId) -> Result<(&Track, &Take), EditError> {
        let found = self.tracks.iter().find_map(|track| Some((track, track.take(id)?)));
        found.ok_or_else(|| {
            let known: Vec<String> = self
                .tracks
                .iter()
                .flat_map(|track| track.takes.iter().map(|take| format!("{} ({})", take.clip.id, take.clip.name)))
                .collect();
            EditError::Invalid(match known.is_empty() {
                true => format!("there is no take {id}: the song has no takes"),
                false => format!("there is no take {id}; the takes are {}", known.join(", ")),
            })
        })
    }

    /// Keeps a recording beside `track` as a take, in the first lane that
    /// has room for it, and returns its id. It is not played: see
    /// [`Project::comp_take`] and [`Project::choose_take`].
    pub fn add_take(
        &mut self,
        track: TrackId,
        name: impl Into<String>,
        start: Beats,
        length: Beats,
        audio: AudioContent,
    ) -> Result<ClipId, EditError> {
        if !start.is_finite() || start < 0.0 || !length.is_finite() || length <= 0.0 {
            return invalid(format!(
                "a take lies somewhere in the song and has a length: not at {start} for {length} beats"
            ));
        }
        let found = self.track(track)?;
        if found.kind != TrackKind::Normal {
            return invalid(format!("{track} ({}) does not hold clips, so it keeps no takes", found.name));
        }
        let id = ClipId(self.alloc_id());
        let audio = AudioContent { crossfade: false, ..audio };
        let track = self.track_mut(track)?;
        let lane = track.free_lane(start, start + length);
        let clip = Clip { id, name: name.into(), start, length, muted: false, content: ClipContent::Audio(audio) };
        track.takes.push(Take { lane, clip });
        Ok(id)
    }

    /// Takes a take out of its lane, and returns it. Its recording stays
    /// where it is on disk, and what the track plays of it stays on the
    /// track: those are clips of their own.
    pub fn remove_take(&mut self, id: ClipId) -> Result<Take, EditError> {
        let track = self.take(id)?.0.id;
        let takes = &mut self.track_mut(track)?.takes;
        let index = takes.iter().position(|take| take.clip.id == id).expect("the take was found on this track");
        Ok(takes.remove(index))
    }

    pub fn rename_take(&mut self, id: ClipId, name: impl Into<String>) -> Result<(), EditError> {
        let name = name.into();
        if name.trim().is_empty() {
            return invalid("a take needs a name");
        }
        let track = self.take(id)?.0.id;
        let take = self.track_mut(track)?.takes.iter_mut().find(|take| take.clip.id == id);
        take.expect("the take was found on this track").clip.name = name;
        Ok(())
    }

    /// Makes room on `track` from `from` to `to`: what its audio clips play
    /// there is taken off the track. A clip that loses anything is kept as
    /// a take first, whole, unless a take that plays the same recording at
    /// the same place is there already. Returns how many clips were cut or
    /// taken off. Notes clips are left alone: they play an instrument, and
    /// audio does not cover them.
    pub fn cover(&mut self, track: TrackId, from: Beats, to: Beats) -> Result<usize, EditError> {
        let map = self.tempo_map();
        let under: Vec<Clip> = self
            .track(track)?
            .clips
            .iter()
            .filter(|clip| clip.audio().is_some() && clip.start < to - TOUCHING && clip.end() > from + TOUCHING)
            .cloned()
            .collect();
        for clip in &under {
            let found = self.track(track)?;
            if !found.takes.iter().any(|take| keeps(&take.clip, clip, &map)) {
                let kept = Clip { id: ClipId(self.alloc_id()), ..clip.clone() };
                let found = self.track_mut(track)?;
                let lane = found.free_lane(kept.start, kept.end());
                found.takes.push(Take { lane, clip: kept });
            }
            let (starts_before, ends_after) = (clip.start < from - TOUCHING, clip.end() > to + TOUCHING);
            match (starts_before, ends_after) {
                (false, false) => {
                    self.remove_clip(clip.id)?;
                }
                (true, true) => {
                    // The part after is a clip of its own from here on.
                    let after = self.split_clip(clip.id, to)?;
                    if let ClipContent::Audio(audio) = &mut self.clip_mut(after)?.content {
                        audio.crossfade = true;
                    }
                    self.clip_mut(clip.id)?.length = from - clip.start;
                }
                (true, false) => {
                    let left = self.clip_mut(clip.id)?;
                    left.length = from - left.start;
                    if let ClipContent::Audio(audio) = &mut left.content {
                        audio.fade_out = 0.0;
                    }
                }
                (false, true) => {
                    let right = self.clip_mut(clip.id)?;
                    let end = right.end();
                    right.content.advance(to - right.start, right.start, &map);
                    (right.start, right.length) = (to, end - to);
                    if let ClipContent::Audio(audio) = &mut right.content {
                        (audio.fade_in, audio.crossfade) = (0.0, true);
                    }
                }
            }
        }
        if !under.is_empty() {
            sort_by_start(&mut self.track_mut(track)?.clips);
        }
        Ok(under.len())
    }

    /// Puts the stretch of a take from `from` to `to` on its track, as an
    /// audio clip that plays there what the take plays there, in place of
    /// what the track had ([`Project::cover`]). Where the new clip meets
    /// an audio clip on either side the two are marked to be faded across
    /// one another. A stretch that reaches past the take is cut to it.
    /// Returns the new clip's id.
    pub fn comp_take(&mut self, id: ClipId, from: Beats, to: Beats) -> Result<ClipId, EditError> {
        let map = self.tempo_map();
        let (track, take) = self.take(id)?;
        let (track, take) = (track.id, take.clip.clone());
        if !from.is_finite() || !to.is_finite() {
            return invalid(format!("a stretch of a take is from one beat to another, not {from} to {to}"));
        }
        let (from, to) = (from.max(take.start), to.min(take.end()));
        if to - from < SHORTEST {
            return invalid(format!(
                "the stretch is not inside the take, which lies from beat {} to beat {}",
                take.start,
                take.end()
            ));
        }
        self.cover(track, from, to)?;
        let new = ClipId(self.alloc_id());
        let mut clip = Clip { id: new, start: from, length: to - from, muted: false, ..take.clone() };
        clip.content.advance(from - take.start, take.start, &map);
        let clips = &mut self.track_mut(track)?.clips;
        let meets = |edge: Beats, at: Beats| (edge - at).abs() <= TOUCHING;
        if let ClipContent::Audio(audio) = &mut clip.content {
            (audio.fade_in, audio.fade_out) = (0.0, 0.0);
            audio.crossfade = clips.iter().any(|other| other.audio().is_some() && meets(other.end(), from));
        }
        for other in clips.iter_mut().filter(|other| meets(other.start, to)) {
            if let ClipContent::Audio(audio) = &mut other.content {
                (audio.fade_in, audio.crossfade) = (0.0, true);
            }
        }
        // And what ends where the new clip begins ends there without a
        // fade of its own: the two are faded across one another.
        for other in clips.iter_mut().filter(|other| meets(other.end(), from)) {
            if let ClipContent::Audio(audio) = &mut other.content {
                audio.fade_out = 0.0;
            }
        }
        clips.push(clip);
        sort_by_start(clips);
        Ok(new)
    }

    /// Puts the whole of a take on its track: [`Project::comp_take`] over
    /// all of it.
    pub fn choose_take(&mut self, id: ClipId) -> Result<ClipId, EditError> {
        let take = &self.take(id)?.1.clip;
        let (start, end) = (take.start, take.end());
        self.comp_take(id, start, end)
    }
}

fn sort_by_start(clips: &mut [Clip]) {
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
}

fn invalid<T>(message: impl Into<String>) -> Result<T, EditError> {
    Err(EditError::Invalid(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Reference;

    fn recording(path: &str) -> AudioContent {
        AudioContent::new(path)
    }

    /// What a track's clips play: `(start, end, path, offset in seconds,
    /// faded across the seam before it)`.
    fn played(project: &Project, track: TrackId) -> Vec<(f64, f64, String, f64, bool)> {
        let clips = &project.track(track).unwrap().clips;
        clips
            .iter()
            .filter_map(|clip| {
                let audio = clip.audio()?;
                Some((clip.start, clip.end(), audio.path.clone(), audio.offset_seconds, audio.crossfade))
            })
            .collect()
    }

    fn takes(project: &Project, track: TrackId) -> Vec<(u32, f64, f64, String)> {
        let takes = &project.track(track).unwrap().takes;
        takes
            .iter()
            .map(|take| (take.lane, take.clip.start, take.clip.end(), take.clip.audio().unwrap().path.clone()))
            .collect()
    }

    #[test]
    fn a_track_says_which_input_it_records_and_what_cannot_record_says_why() {
        let mut project = Project::new("Song");
        let voice = project.add_track("Voice");
        assert_eq!(project.track(voice).unwrap().input, None, "a track records a keyboard until it says otherwise");
        let pair = TrackInput { channel: 3, stereo: true, monitor: Monitoring::Armed };
        project.set_track_input(voice, Some(pair)).unwrap();
        assert_eq!(project.track(voice).unwrap().input, Some(pair));
        assert_eq!(pair.channels(), (2, Some(3)));
        assert_eq!((pair.said(), TrackInput::mono(1).said()), ("inputs 3 and 4".to_string(), "input 1".to_string()));
        project.set_track_input(voice, None).unwrap();
        assert_eq!(project.track(voice).unwrap().input, None);

        let group = project.add_track_of_kind("Band", TrackKind::Group);
        let error = project.set_track_input(group, Some(TrackInput::mono(1))).unwrap_err().to_string();
        assert!(error.contains("group track") && error.contains("Band"), "{error}");
        let error = project.set_track_input(voice, Some(TrackInput::mono(0))).unwrap_err().to_string();
        assert!(error.contains("channels 1 to 32"), "{error}");
        let wide = TrackInput { channel: 32, stereo: true, monitor: Monitoring::Off };
        assert!(project.set_track_input(voice, Some(wide)).is_err());
        project.set_reference(voice, Some(Reference::default())).unwrap();
        let error = project.set_track_input(voice, Some(TrackInput::mono(1))).unwrap_err().to_string();
        assert!(error.contains("reference"), "{error}");
    }

    #[test]
    fn a_file_that_says_nothing_of_inputs_or_takes_is_the_file_it_always_was() {
        let mut project = Project::new("Song");
        let track = project.add_track("Keys");
        project.add_audio_clip(track, "Loop", 0.0, 4.0, recording("loop.wav")).unwrap();
        let text = project.to_json();
        for word in ["input", "takes", "crossfade", "monitor"] {
            assert!(!text.contains(word), "a song without {word} does not write it");
        }
        assert_eq!(Project::from_json(&text).unwrap(), project);

        project
            .set_track_input(track, Some(TrackInput { channel: 2, stereo: false, monitor: Monitoring::Always }))
            .unwrap();
        let take = project.add_take(track, "Take 1", 4.0, 4.0, recording("take 1.wav")).unwrap();
        project.choose_take(take).unwrap();
        let back = Project::from_json(&project.to_json()).unwrap();
        assert_eq!(back, project);
        assert_eq!(back.track(track).unwrap().takes.len(), 1);
    }

    #[test]
    fn takes_lie_in_lanes_beside_the_track_and_are_not_among_its_clips() {
        let mut project = Project::new("Song");
        let voice = project.add_track("Voice");
        let first = project.add_take(voice, "Take 1", 8.0, 8.0, recording("a.wav")).unwrap();
        let second = project.add_take(voice, "Take 2", 8.0, 8.0, recording("b.wav")).unwrap();
        let later = project.add_take(voice, "Take 3", 32.0, 4.0, recording("c.wav")).unwrap();
        assert_eq!(takes(&project, voice).iter().map(|take| take.0).collect::<Vec<_>>(), [0, 1, 0]);
        assert_eq!(project.track(voice).unwrap().lanes(), 2);
        assert!(project.track(voice).unwrap().clips.is_empty(), "a take is not played");
        assert!(project.clip(first).is_err(), "and is not a clip of the track");
        assert_eq!(project.length_beats(), 0.0, "nor does it make the song longer");
        assert_eq!(project.take(second).unwrap().1.clip.name, "Take 2");

        project.rename_take(second, "The good one").unwrap();
        assert_eq!(project.take(second).unwrap().1.clip.name, "The good one");
        assert!(project.rename_take(second, "  ").is_err());
        let gone = project.remove_take(first).unwrap();
        assert_eq!(gone.clip.id, first);
        let error = project.take(first).unwrap_err().to_string();
        assert!(error.contains(&later.to_string()) && error.contains("The good one"), "{error}");
        let group = project.add_track_of_kind("Band", TrackKind::Group);
        assert!(project.add_take(group, "No", 0.0, 4.0, recording("a.wav")).is_err());
    }

    #[test]
    fn a_stretch_of_a_take_takes_that_stretchs_place_and_the_seams_are_marked() {
        let mut project = Project::new("Song");
        let voice = project.add_track("Voice");
        let first = project.add_take(voice, "Take 1", 8.0, 8.0, recording("a.wav")).unwrap();
        let second = project.add_take(voice, "Take 2", 8.0, 8.0, recording("b.wav")).unwrap();
        project.choose_take(first).unwrap();
        assert_eq!(played(&project, voice), [(8.0, 16.0, "a.wav".to_string(), 0.0, false)]);

        // Two beats of the second, in the middle. At 120 beats a minute a
        // beat is half a second of a recording.
        let piece = project.comp_take(second, 10.0, 12.0).unwrap();
        assert_eq!(
            played(&project, voice),
            [
                (8.0, 10.0, "a.wav".to_string(), 0.0, false),
                (10.0, 12.0, "b.wav".to_string(), 1.0, true),
                (12.0, 16.0, "a.wav".to_string(), 2.0, true),
            ]
        );
        assert_eq!(project.clip(piece).unwrap().name, "Take 2");
        // Nothing was lost, so nothing more was kept: both are in their lanes.
        assert_eq!(takes(&project, voice).len(), 2);

        // A stretch that reaches past the take is cut to it, and one that
        // is nowhere in it is refused.
        project.comp_take(second, 14.0, 99.0).unwrap();
        assert_eq!(played(&project, voice).last().unwrap(), &(14.0, 16.0, "b.wav".to_string(), 3.0, true));
        let error = project.comp_take(second, 20.0, 24.0).unwrap_err().to_string();
        assert!(error.contains("from beat 8 to beat 16"), "{error}");

        // The whole of the first again: one clip, as it was.
        project.choose_take(first).unwrap();
        assert_eq!(played(&project, voice), [(8.0, 16.0, "a.wav".to_string(), 0.0, false)]);
        assert_eq!(takes(&project, voice).len(), 2);
    }

    #[test]
    fn what_a_take_covers_is_kept_as_a_take_first_unless_it_is_one_already() {
        let mut project = Project::new("Song");
        let voice = project.add_track("Voice");
        // A clip that was on the track before anything was recorded, with
        // a fade at each end, and a notes clip under the same stretch.
        let content = AudioContent { offset_seconds: 0.5, fade_in: 0.5, fade_out: 0.5, ..recording("sung before.wav") };
        let old = project.add_audio_clip(voice, "Sung before", 4.0, 16.0, content.clone()).unwrap();
        let notes = project.add_midi_clip(voice, "Keys", 8.0, 4.0).unwrap();
        let take = project.add_take(voice, "Take 1", 8.0, 4.0, recording("take.wav")).unwrap();
        project.choose_take(take).unwrap();

        // The old clip is in a lane, whole and as it was, and the track
        // plays it on either side of the take.
        let kept: Vec<&Take> =
            project.track(voice).unwrap().takes.iter().filter(|take| take.clip.name == "Sung before").collect();
        assert_eq!(kept.len(), 1);
        assert_eq!((kept[0].clip.start, kept[0].clip.length), (4.0, 16.0));
        assert_eq!(kept[0].clip.audio(), Some(&content));
        assert_eq!(kept[0].lane, 1, "in a lane of its own: the take lies over the same stretch");
        assert!(project.clip(notes).is_ok(), "notes are not covered by audio");
        let audio: Vec<_> = played(&project, voice)
            .into_iter()
            .map(|(start, end, path, offset, _)| (start, end, path, offset))
            .collect();
        assert_eq!(
            audio,
            [
                (4.0, 8.0, "sung before.wav".to_string(), 0.5),
                (8.0, 12.0, "take.wav".to_string(), 0.0),
                (12.0, 20.0, "sung before.wav".to_string(), 4.5),
            ]
        );
        // The parts keep the fades of the edges that are still theirs.
        let left = project.clip(old).unwrap().audio().unwrap();
        assert_eq!((left.fade_in, left.fade_out), (0.5, 0.0));

        // Covering more of it keeps nothing new: the lane has it.
        let again = project.add_take(voice, "Take 2", 12.0, 4.0, recording("take 2.wav")).unwrap();
        project.choose_take(again).unwrap();
        assert_eq!(project.track(voice).unwrap().takes.len(), 3, "two takes and the clip that was there");

        // And the old clip can be had back whole.
        let kept = kept_id(&project, voice, "Sung before");
        project.choose_take(kept).unwrap();
        let audio = played(&project, voice);
        assert_eq!(audio.len(), 1);
        assert_eq!((audio[0].0, audio[0].1, audio[0].3), (4.0, 20.0, 0.5));
        assert_eq!(project.track(voice).unwrap().takes.len(), 3, "nothing was lost, so nothing was kept again");
    }

    fn kept_id(project: &Project, track: TrackId, name: &str) -> ClipId {
        project.track(track).unwrap().takes.iter().find(|take| take.clip.name == name).unwrap().clip.id
    }

    #[test]
    fn a_take_that_covers_the_start_or_the_end_of_a_clip_leaves_the_rest() {
        let mut project = Project::new("Song");
        project.set_tempo(60.0).unwrap();
        let voice = project.add_track("Voice");
        project.add_audio_clip(voice, "Before", 0.0, 8.0, recording("before.wav")).unwrap();
        project.add_audio_clip(voice, "After", 8.0, 8.0, recording("after.wav")).unwrap();
        let take = project.add_take(voice, "Take", 6.0, 4.0, recording("take.wav")).unwrap();
        project.choose_take(take).unwrap();
        assert_eq!(
            played(&project, voice),
            [
                (0.0, 6.0, "before.wav".to_string(), 0.0, false),
                (6.0, 10.0, "take.wav".to_string(), 0.0, true),
                // A beat is a second at this tempo: two of them in.
                (10.0, 16.0, "after.wav".to_string(), 2.0, true),
            ]
        );
        assert_eq!(project.track(voice).unwrap().takes.len(), 3);
        // A duplicate of the track has takes of its own.
        let copy = project.duplicate_track(voice).unwrap();
        let (original, copied) = (project.track(voice).unwrap(), project.track(copy).unwrap());
        assert_eq!(copied.takes.len(), 3);
        assert!(copied.takes.iter().all(|take| original.take(take.clip.id).is_none()), "with ids of their own");
    }
}
