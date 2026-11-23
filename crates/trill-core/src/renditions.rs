//! A track heard where its plugins are not: a *rendition*.
//!
//! In a song that several programs hold, one of them may have a plugin
//! that another has not. The one that has it records what the track's
//! devices make of the track, up to and including the last one that
//! someone lacks, and that recording is the song's: a [`Rendition`]. A
//! program that has not got one of those devices plays the recording in
//! their place, and runs what follows it on the track itself. So the
//! track's fader, its pan, its sends and whatever effects it does have
//! after that device are still everyone's to move, and are heard at once;
//! what waits for a program that has the devices is a change to the notes
//! or to the devices the recording stands in for.
//!
//! A rendition says what the song was when it was made, as far as the
//! recording depends on it ([`Project::rendition_print`]): the track's
//! clips and notes, the devices it stands in for, the envelopes that move
//! them, the tempo. While that is what the song still is, the rendition
//! is the track as it sounds. When it is not, the rendition is of the song
//! as it was, which is said to whoever hears it, and a program that has
//! the devices makes it again.
//!
//! It is of a track of the song that plays clips. A group, a return and
//! the master have none: what they play is what other tracks feed them,
//! and a recording of that would stand in for every fader on the way.
//! A device that is missing there lets through what comes into it, as it
//! always did.
//!
//! Like every file of a shared song, the recording is the song's by what
//! it is ([`FileIs`]) and where it lies is each machine's own
//! ([`Track::rendition_at`]): see [`files`](crate::files).

use crate::files::FileIs;
use crate::instrument::InstrumentProfile;
use crate::project::{AutomationLane, AutomationTarget, Clip, ClipContent, Device, EditError, TrackKind};
use crate::tempo::{MeterChange, TempoChange};
use crate::time::TimeSignature;
use crate::{Beats, DeviceId, Project, Track, TrackId};
use serde::{Deserialize, Serialize};

/// The most letters the name of whoever made a rendition is kept at.
const LONGEST_NAME: usize = 64;

/// What a track's first devices make of it, recorded by a program that
/// has them: see the [module](self).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rendition {
    /// The device whose output it is: the recording stands in for that
    /// device and for every device before it on the track.
    pub after: DeviceId,
    /// What the song was when it was made, as far as the recording
    /// depends on it: [`Project::rendition_print`], then.
    pub of: String,
    /// What the recording is.
    pub is: FileIs,
    /// Who made it, as they go by. For a sentence, and for nothing else.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    /// Where in the song the recording begins, in beats: where the track
    /// first plays anything. Nothing is recorded of the bars before.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub from: Beats,
}

fn is_zero(beats: &Beats) -> bool {
    *beats == 0.0
}

/// Everything a rendition depends on, as it is written out to be told
/// apart from what it was: the order of these is what a print is of, and
/// two builds that write it differently take each other's renditions for
/// ones of the song as it was (which costs making them again, and nothing
/// else).
#[derive(Serialize)]
struct Heard<'a> {
    tempo: f64,
    time_signature: TimeSignature,
    tempo_changes: &'a [TempoChange],
    meter_changes: &'a [MeterChange],
    /// The track's clips, without where their files lie.
    clips: Vec<Clip>,
    profile: &'a InstrumentProfile,
    /// The devices it stands in for.
    devices: &'a [Device],
    /// The envelopes that move them.
    envelopes: Vec<&'a AutomationLane>,
    /// Where one of them listens to another track: every track that is
    /// played before it, and every envelope of the song.
    before: Vec<Track>,
    all_envelopes: &'a [AutomationLane],
}

/// A short text that is the same for the same bytes: no secret, only a
/// way to tell two songs apart.
fn print_of(bytes: &[u8]) -> String {
    let pass = |mut hash: u64| {
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    };
    format!("{:016x}{:016x}", pass(0xcbf2_9ce4_8422_2325), pass(0x6c62_272e_07bb_0142))
}

/// A clip as a rendition depends on it: without where its file lies,
/// which is each machine's own.
fn placeless(clip: &Clip) -> Clip {
    let mut clip = clip.clone();
    if let ClipContent::Audio(audio) = &mut clip.content {
        audio.path.clear();
    }
    clip
}

impl Track {
    /// The recording this machine plays for the track's rendition, if it
    /// has one to play: the rendition's own once that is here, and until
    /// then the one it had before, so that a track does not fall silent
    /// for as long as a newer recording takes to arrive. Whether the file
    /// is there is for whoever reads it to find.
    pub fn rendition_here(&self) -> Option<&str> {
        self.rendition.as_ref()?;
        (!self.rendition_at.is_empty()).then_some(self.rendition_at.as_str())
    }

    /// How many of the track's devices its rendition stands in for: the
    /// device it was recorded after, and every device before that one.
    /// `None` where the track has no rendition, or that device is no
    /// longer on it.
    pub fn stood_in(&self) -> Option<usize> {
        let after = self.rendition.as_ref()?.after;
        self.devices.iter().position(|device| device.id == after).map(|at| at + 1)
    }
}

impl Project {
    /// What a recording of `track` as it leaves its device `after` depends
    /// on, as a short text: the same in every program that holds the same
    /// song, and another as soon as anything changes that such a recording
    /// would come out differently for. The track's clips with their notes
    /// and curves, what its notes mean, the devices up to `after` with
    /// their settings, the envelopes that move those, and the song's tempo
    /// and time signature with their changes. Where one of those devices
    /// listens to another track, everything that is played before this
    /// track too. Not the track's fader, pan, mute or sends, which come
    /// after, nor anything that is one person's own.
    ///
    /// `None` where there is no such track, or the device is not on it.
    ///
    /// Asked of the song as it is said to others (without where a file
    /// lies, also in a built-in device's state), it is the same on every
    /// machine: a place is each machine's own.
    pub fn rendition_print(&self, track: TrackId, after: DeviceId) -> Option<String> {
        let track = self.track(track).ok()?;
        let count = track.devices.iter().position(|device| device.id == after)? + 1;
        let devices = &track.devices[..count];
        let moved = |lane: &&AutomationLane| match &lane.target {
            AutomationTarget::DeviceParam { device, .. } => devices.iter().any(|held| held.id == *device),
            _ => false,
        };
        let listens = devices.iter().any(|device| device.sidechain.is_some());
        let before: Vec<Track> = match listens {
            true => self.played_before(track.id).into_iter().map(|id| self.as_it_sounds(id)).collect(),
            false => Vec::new(),
        };
        let heard = Heard {
            tempo: self.tempo,
            time_signature: self.time_signature,
            tempo_changes: &self.tempo_changes,
            meter_changes: &self.meter_changes,
            clips: track.clips.iter().map(placeless).collect(),
            profile: &track.profile,
            devices,
            envelopes: self.automation.iter().filter(moved).collect(),
            before,
            all_envelopes: if listens { &self.automation } else { &[] },
        };
        Some(print_of(&serde_json::to_vec(&heard).ok()?))
    }

    /// Every track that is played before `track` can be: what feeds it,
    /// and what feeds those. In the order of the track list.
    fn played_before(&self, track: TrackId) -> Vec<TrackId> {
        let mut found: Vec<TrackId> = Vec::new();
        let mut look = self.feeds(track);
        while let Some(next) = look.pop() {
            if next != track && !found.contains(&next) {
                found.push(next);
                look.extend(self.feeds(next));
            }
        }
        found.sort_by_key(|id| self.track_index(*id).unwrap_or(usize::MAX));
        found
    }

    /// A track as it sounds, for a print: without what is one person's
    /// own, where its files lie, its takes and its own rendition.
    fn as_it_sounds(&self, id: TrackId) -> Track {
        let Ok(track) = self.track(id) else { return self.tracks[0].clone() };
        Track {
            solo: false,
            input: None,
            clips: track.clips.iter().map(placeless).collect(),
            takes: Vec::new(),
            rendition: None,
            rendition_at: String::new(),
            ..track.clone()
        }
    }

    /// The devices a program has to have to make a recording of `track`
    /// as it leaves its device `after`: the track's devices up to that
    /// one, and every device of the tracks that are played before it.
    /// Empty where there is no such track or device.
    pub fn rendition_takes(&self, track: TrackId, after: DeviceId) -> Vec<DeviceId> {
        let Ok(held) = self.track(track) else { return Vec::new() };
        let Some(at) = held.devices.iter().position(|device| device.id == after) else { return Vec::new() };
        let mut takes: Vec<DeviceId> = held.devices[..=at].iter().map(|device| device.id).collect();
        for before in self.played_before(track) {
            if let Ok(before) = self.track(before) {
                takes.extend(before.devices.iter().map(|device| device.id));
            }
        }
        takes
    }

    /// The stretch of the song in which `track` plays anything of its
    /// own, in beats: from where its first clip that is not muted begins
    /// to where the last ends. `None` for a track that plays nothing.
    pub fn stretch_played(&self, track: TrackId) -> Option<(Beats, Beats)> {
        let playing = self.track(track).ok()?.clips.iter().filter(|clip| !clip.muted);
        let (from, to) = playing
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(from, to), clip| (from.min(clip.start), to.max(clip.end())));
        (to > from).then_some((from.max(0.0), to))
    }

    /// Whether `track` can have a rendition: it is a track of the song
    /// that plays clips of its own.
    pub fn can_be_rendered(&self, track: TrackId) -> Result<(), EditError> {
        let held = self.track(track)?;
        if held.kind != TrackKind::Normal {
            let kind = if held.kind == TrackKind::Group { "group" } else { "return" };
            return Err(EditError::Invalid(format!(
                "{track} is a {kind}: what it plays is what other tracks feed it, and no recording stands in for that"
            )));
        }
        if held.reference.is_some() {
            return Err(EditError::Invalid(format!("{track} is a reference: it is a recording already")));
        }
        Ok(())
    }

    /// Gives `track` a rendition, or with `None` takes the one it has
    /// away. The device it was recorded after has to be on the track.
    /// Which recording this machine plays for it is said apart
    /// ([`Track::rendition_at`]) and stays as it is, the one before a
    /// newer rendition until that one is here; a track without a rendition
    /// plays none.
    pub fn set_rendition(&mut self, track: TrackId, rendition: Option<Rendition>) -> Result<(), EditError> {
        if let Some(rendition) = &rendition {
            self.can_be_rendered(track)?;
            if !self.track(track)?.devices.iter().any(|device| device.id == rendition.after) {
                return Err(EditError::DeviceNotFound(rendition.after));
            }
            if !rendition.is.is_well_formed() || rendition.of.is_empty() {
                return Err(EditError::Invalid(
                    "a rendition says what its recording is and what it was made of".into(),
                ));
            }
            if !rendition.from.is_finite() || rendition.from < 0.0 {
                return Err(EditError::Invalid("a rendition begins at a beat of the song".into()));
            }
        }
        let held = self.track_mut(track)?;
        if rendition.is_none() {
            held.rendition_at.clear();
        }
        held.rendition = rendition.map(|mut rendition| {
            if rendition.by.len() > LONGEST_NAME {
                let end = (0..=LONGEST_NAME).rev().find(|end| rendition.by.is_char_boundary(*end)).unwrap_or(0);
                rendition.by.truncate(end);
            }
            rendition
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChainRef;
    use crate::files::Content;
    use crate::merge::lot::note;
    use crate::project::{AudioContent, AutomationPoint};

    /// A song with a bass that plays through a synth and a filter, and a
    /// kick the filter may be made to listen to.
    fn song() -> (Project, TrackId, DeviceId, DeviceId, TrackId) {
        let mut song = Project::new("Song");
        let bass = song.add_track("Bass");
        let kick = song.add_track("Kick");
        let synth = song.add_device(ChainRef::Track(bass), "clap:a.synth", "Synth", None).unwrap();
        let filter = song.add_device(ChainRef::Track(bass), "builtin:filter", "Filter", None).unwrap();
        song.add_device(ChainRef::Track(kick), "builtin:drums", "Drums", None).unwrap();
        let line = song.add_midi_clip(bass, "Line", 0.0, 8.0).unwrap();
        song.clip_mut(line).unwrap().add_notes(&[note(36, 0.0), note(43, 2.0)]).unwrap();
        let hits = song.add_midi_clip(kick, "Hits", 0.0, 8.0).unwrap();
        song.clip_mut(hits).unwrap().add_notes(&[note(36, 0.0)]).unwrap();
        (song, bass, synth, filter, kick)
    }

    fn rendition(after: DeviceId, of: &str) -> Rendition {
        let is = FileIs::new(Content::new(&[7; 32], 1000), "Bass.wav");
        Rendition { after, of: of.to_owned(), is, by: "Ada".into(), from: 0.0 }
    }

    #[test]
    fn a_print_is_another_for_whatever_the_recording_depends_on_and_the_same_for_what_comes_after() {
        let (song, bass, synth, filter, kick) = song();
        let print = |song: &Project| song.rendition_print(bass, synth).unwrap();
        let was = print(&song);
        assert_eq!(was.len(), 32);
        assert_eq!(print(&song.clone()), was, "the same song, the same print");
        assert_ne!(song.rendition_print(bass, filter).unwrap(), was, "after another device, another recording");
        assert_eq!(song.rendition_print(bass, DeviceId(999)), None);
        assert_eq!(song.rendition_print(TrackId(999), synth), None);

        // What comes after the device, and what is beside the track.
        let mut after = song.clone();
        after.set_track_volume(bass, -9.0).unwrap();
        after.set_track_pan(bass, 0.5).unwrap();
        after.track_mut(bass).unwrap().mute = true;
        after.track_mut(bass).unwrap().solo = true;
        after.track_mut(bass).unwrap().name = "Sub".into();
        after.device_mut(filter).unwrap().params.insert("cutoff".into(), 900.0);
        after.set_track_volume(kick, -3.0).unwrap();
        let hits = after.track(kick).unwrap().clips[0].id;
        after.clip_mut(hits).unwrap().add_notes(&[note(38, 1.0)]).unwrap();
        assert_eq!(print(&after), was, "none of it is in what leaves the synth");
        // Nor its own rendition, nor where anything lies on this machine.
        after.set_rendition(bass, Some(rendition(synth, &was))).unwrap();
        after.track_mut(bass).unwrap().rendition_at = "C:/somewhere/Bass.wav".into();
        assert_eq!(print(&after), was);

        // What the recording is made of.
        let changed = |edit: &dyn Fn(&mut Project)| {
            let mut song = song.clone();
            edit(&mut song);
            print(&song)
        };
        let line = song.track(bass).unwrap().clips[0].id;
        assert_ne!(changed(&|song| song.clip_mut(line).unwrap().add_notes(&[note(48, 4.0)]).unwrap()), was);
        assert_ne!(changed(&|song| song.move_clip(line, None, 4.0).unwrap()), was);
        assert_ne!(changed(&|song| song.clip_mut(line).unwrap().muted = true), was);
        let set = |song: &mut Project| {
            song.device_mut(synth).unwrap().params.insert("cutoff".into(), 400.0);
        };
        assert_ne!(changed(&set), was);
        assert_ne!(changed(&|song| song.device_mut(synth).unwrap().state = Some(vec![1, 2, 3])), was);
        assert_ne!(changed(&|song| song.device_mut(synth).unwrap().enabled = false), was);
        assert_ne!(changed(&|song| song.set_tempo(98.0).unwrap()), was);
        let faster = TempoChange { beat: 4.0, tempo: 140.0, ramp: false };
        assert_ne!(changed(&|song| song.set_tempo_changes(vec![faster]).unwrap()), was);
        let envelope = AutomationTarget::DeviceParam { device: synth, param: "cutoff".into() };
        let points = vec![AutomationPoint::new(0.0, 200.0), AutomationPoint::new(8.0, 2000.0)];
        let moved = changed(&|song| song.set_automation(envelope.clone(), points.clone()).unwrap());
        assert_ne!(moved, was);
        let elsewhere = AutomationTarget::DeviceParam { device: filter, param: "cutoff".into() };
        assert_eq!(changed(&|song| song.set_automation(elsewhere.clone(), points.clone()).unwrap()), was);

        // An audio clip by what its file is, wherever it lies.
        let mut with_audio = song.clone();
        let clip = with_audio.add_audio_clip(bass, "Sub", 8.0, 4.0, AudioContent::new("C:/here/sub.wav")).unwrap();
        let here = print(&with_audio);
        assert_ne!(here, was);
        if let ClipContent::Audio(audio) = &mut with_audio.clip_mut(clip).unwrap().content {
            audio.path = "D:/there/sub.wav".into();
        }
        assert_eq!(print(&with_audio), here, "a place is each machine's own");
    }

    #[test]
    fn where_a_device_listens_to_another_track_the_recording_depends_on_that_track_too() {
        let (mut song, bass, _, filter, kick) = song();
        let alone = song.rendition_print(bass, filter).unwrap();
        assert_eq!(song.rendition_takes(bass, filter).len(), 2, "the bass's two devices");
        song.set_sidechain(filter, Some(kick)).unwrap();
        let keyed = song.rendition_print(bass, filter).unwrap();
        assert_ne!(keyed, alone);
        assert_eq!(song.rendition_takes(bass, filter).len(), 3, "and the kick's, which is played before it");
        let hits = song.track(kick).unwrap().clips[0].id;
        song.clip_mut(hits).unwrap().add_notes(&[note(38, 1.0)]).unwrap();
        assert_ne!(
            song.rendition_print(bass, filter).unwrap(),
            keyed,
            "what the kick plays is in what the filter does"
        );
    }

    #[test]
    fn a_rendition_is_of_a_track_that_plays_clips_after_a_device_that_is_on_it() {
        let (mut song, bass, synth, filter, kick) = song();
        let drums = song.track(kick).unwrap().devices[0].id;
        assert_eq!(song.track(bass).unwrap().stood_in(), None);
        song.set_rendition(bass, Some(rendition(synth, "abc"))).unwrap();
        assert_eq!(song.track(bass).unwrap().stood_in(), Some(1));
        assert_eq!(song.track(bass).unwrap().rendition_here(), None, "nothing to play until a recording is here");
        song.track_mut(bass).unwrap().rendition_at = "C:/here/Bass.wav".into();
        assert_eq!(song.track(bass).unwrap().rendition_here(), Some("C:/here/Bass.wav"));
        // A newer rendition: the recording that is here goes on being
        // played until whoever has the newer one says it is here.
        let mut other = rendition(filter, "abe");
        other.is = FileIs::new(Content::new(&[8; 32], 2000), "Bass.wav");
        song.set_rendition(bass, Some(other)).unwrap();
        assert_eq!(song.track(bass).unwrap().stood_in(), Some(2));
        assert_eq!(song.track(bass).unwrap().rendition_here(), Some("C:/here/Bass.wav"));

        // Not after a device of another track, not without saying what it
        // is, not for a group or a return.
        assert_eq!(song.set_rendition(bass, Some(rendition(drums, "abc"))), Err(EditError::DeviceNotFound(drums)));
        assert!(song.set_rendition(bass, Some(rendition(synth, ""))).is_err());
        let room = song.add_track_of_kind("Room", TrackKind::Return);
        let verb = song.add_device(ChainRef::Track(room), "clap:a.reverb", "Reverb", None).unwrap();
        let refused = song.set_rendition(room, Some(rendition(verb, "abc"))).unwrap_err().to_string();
        assert!(refused.contains("is a return"), "{refused}");
        // It begins where the song has a beat.
        let nowhere = Rendition { from: f64::NAN, ..rendition(synth, "abc") };
        assert!(song.set_rendition(bass, Some(nowhere)).is_err());
        assert_eq!(song.stretch_played(bass), Some((0.0, 8.0)));
        let line = song.track(bass).unwrap().clips[0].id;
        song.move_clip(line, None, 16.0).unwrap();
        assert_eq!(song.stretch_played(bass), Some((16.0, 24.0)), "nothing is recorded of the bars before");
        song.clip_mut(line).unwrap().muted = true;
        assert_eq!(song.stretch_played(bass), None, "a track that plays nothing has nothing to record");
        // The device goes: the rendition stands in for nothing.
        song.remove_device(filter).unwrap();
        assert_eq!(song.track(bass).unwrap().stood_in(), None);
        song.set_rendition(bass, None).unwrap();
        assert_eq!(song.track(bass).unwrap().rendition, None);
        assert_eq!(song.track(bass).unwrap().rendition_here(), None, "and no recording is played for none");
    }
}
