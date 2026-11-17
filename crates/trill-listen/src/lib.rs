//! What can be heard in a recording without a model.
//!
//! Given one sound, or the stems a recording was taken apart into,
//! [`hear`] finds:
//!
//! - the **beat grid**: every beat and bar line, moved onto the hit it
//!   belongs to, and the tempo ([`beats`]), with the corrections for when
//!   the tracker has it wrong;
//! - the **tuning**, the **key** with its runners-up, and a **chord** for
//!   each stretch of beats ([`harmony`]);
//! - the **parts** of the song and which of them come round again
//!   ([`structure`]);
//! - the **loops** worth having, ranked by how well they come back round
//!   ([`loops`]);
//! - the **single hits** of the drums told apart, one bass note for each
//!   pitch, and the drum **groove** ([`hits`]);
//! - the **notes** of the bass line and of a sung melody ([`pitch`]);
//!
//! and [`midi`] writes the notes, the groove and the chords as MIDI on the
//! recording's own beats.
//!
//! Everything works on one file and better on a stem: the drums of a full
//! mix are read through everything else that plays. Nothing here reads a
//! file or keeps anything: a [`Recording`] goes in and a [`Heard`] comes
//! out, the same for the same audio on every machine.
//!
//! The method is the one the owner's Magpie project proved, translated
//! from its Python and redesigned where Trill wanted something else; what
//! Magpie had from libraries of others (the transforms, the beat tracker,
//! the filters) is written here.

pub mod beats;
pub mod harmony;
pub mod hits;
pub mod loops;
pub mod midi;
pub mod onsets;
pub mod pitch;
pub mod signal;
pub mod spectrum;
pub mod structure;

pub use beats::{Correction, Grid, Tracker};
pub use harmony::{Chord, Harmony, Key, KeyReading, KeyScore, Quality};
pub use hits::{GrooveNote, HitKind, OneShot};
pub use loops::Loop;
pub use pitch::VoiceNote;
pub use signal::{RATE, Sound};
pub use structure::Part;

use serde::{Deserialize, Serialize};
use trill_core::midi_file::MidiFile;

/// One stem of a recording that was taken apart.
#[derive(Clone, Debug, PartialEq)]
pub struct Stem {
    /// `vocals`, `drums`, `bass`, `guitar`, `piano`, `other`.
    pub name: String,
    pub sound: Sound,
}

/// What a stem is called by a person.
pub fn stem_label(name: &str) -> String {
    match name {
        "vocals" => "Vocals".to_owned(),
        "drums" => "Drums".to_owned(),
        "bass" => "Bass".to_owned(),
        "guitar" => "Guitar".to_owned(),
        "piano" => "Keys".to_owned(),
        "other" => "Other".to_owned(),
        other => {
            let mut letters = other.chars();
            letters.next().map(|first| first.to_uppercase().chain(letters).collect()).unwrap_or_default()
        }
    }
}

/// A recording to listen to: one sound, or that sound and the stems that
/// add up to it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recording {
    pub mix: Sound,
    pub stems: Vec<Stem>,
}

impl Recording {
    /// A recording that was not taken apart.
    pub fn whole(sound: Sound) -> Self {
        Self { mix: sound, stems: Vec::new() }
    }

    /// A recording from its stems: the mix is what they add up to.
    pub fn of_stems(stems: Vec<Stem>) -> Self {
        let mix = stems
            .iter()
            .skip(1)
            .fold(stems.first().map(|stem| stem.sound.clone()).unwrap_or_default(), |mix, stem| mix.plus(&stem.sound));
        Self { mix, stems }
    }

    pub fn stem(&self, name: &str) -> Option<&Sound> {
        self.stems.iter().find(|stem| stem.name == name).map(|stem| &stem.sound)
    }

    pub fn seconds(&self) -> f64 {
        self.mix.seconds()
    }

    /// The signal whose hits the beats are moved onto: the drums when
    /// there are any to speak of, otherwise everything.
    fn hits(&self) -> Vec<f32> {
        let mix = self.mix.mono();
        if let Some(drums) = self.stem("drums") {
            let drums = drums.mono();
            let (level, whole) = (signal::rms(&drums), signal::rms(&mix));
            if whole > 0.0 && level > 0.05 * whole {
                return drums;
            }
        }
        mix
    }

    /// Everything but the drums, as one channel.
    fn pitched(&self) -> Vec<f32> {
        let others: Vec<&Stem> = self.stems.iter().filter(|stem| stem.name != "drums").collect();
        if others.is_empty() || others.len() == self.stems.len() && self.stems.len() < 2 {
            return self.mix.mono();
        }
        let mut sum = vec![0.0f32; self.mix.frames()];
        for stem in others {
            for (slot, sample) in sum.iter_mut().zip(stem.sound.mono()) {
                *slot += sample;
            }
        }
        sum
    }
}

/// Beats a model found, for [`Options::beats`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackedBeats {
    /// Beat and downbeat times, in seconds.
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
}

/// How to listen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    /// Beats that were found already, by a model that hears them better
    /// than the pulse of the onsets does. Left out, the onsets are followed.
    pub beats: Option<TrackedBeats>,
    /// Corrections to the grid, in the order they are made.
    pub corrections: Vec<Correction>,
}

/// Everything heard in one recording. Times are in seconds from its start.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Heard {
    pub seconds: f64,
    /// The names of the stems it was heard in, or none for one sound.
    pub stems: Vec<String>,
    pub grid: Grid,
    pub harmony: Option<Harmony>,
    pub parts: Vec<Part>,
    pub loops: Vec<Loop>,
    pub one_shots: Vec<OneShot>,
    /// The drum part as notes.
    pub groove: Vec<GrooveNote>,
    /// The notes of the bass line.
    pub bass: Vec<VoiceNote>,
    /// The notes of the sung melody.
    pub melody: Vec<VoiceNote>,
    /// How loud each stem is over time, 0 to 1, every tenth of a second.
    pub activity: Vec<(String, Vec<f32>)>,
}

/// What of a recording can be written as MIDI.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MidiPart {
    Bass,
    Melody,
    Groove,
    Chords,
}

impl MidiPart {
    pub const ALL: [MidiPart; 4] = [MidiPart::Bass, MidiPart::Melody, MidiPart::Groove, MidiPart::Chords];

    pub fn name(self) -> &'static str {
        match self {
            MidiPart::Bass => "Bass line",
            MidiPart::Melody => "Melody",
            MidiPart::Groove => "Drum groove",
            MidiPart::Chords => "Chords",
        }
    }
}

impl Heard {
    /// The key, when there was enough pitched sound to read one.
    pub fn key(&self) -> Option<&KeyReading> {
        self.harmony.as_ref().map(|harmony| &harmony.key).filter(|key| key.score > 0.0)
    }

    /// How notes are spelled in this recording's key.
    pub fn flats(&self) -> bool {
        self.key().is_some_and(|reading| reading.key.uses_flats())
    }

    /// `part` between `start` and `end` seconds as a one-track MIDI file
    /// whose beats are the recording's own, counted from `start`. The
    /// groove's hits are moved onto sixteenths; pitched notes are left as
    /// played unless `quantize` (steps to a beat) says otherwise.
    pub fn midi(&self, part: MidiPart, start: f64, end: f64, quantize: Option<u32>) -> MidiFile {
        let (notes, drums, usual) = match part {
            MidiPart::Bass => (midi::melody(&self.bass, start, end, None), false, 0),
            MidiPart::Melody => (midi::melody(&self.melody, start, end, None), false, 0),
            MidiPart::Groove => (midi::drum_part(&self.groove, start, end), true, 4),
            MidiPart::Chords => (
                midi::chord_part(self.harmony.as_ref().map_or(&[][..], |harmony| &harmony.chords), start, end),
                false,
                0,
            ),
        };
        midi::file(&notes, &self.grid, start, part.name(), drums, quantize.unwrap_or(usual))
    }
}

/// Listens to a recording: see the crate's own words for what is found.
pub fn hear(recording: &Recording, options: &Options) -> Heard {
    let seconds = recording.seconds();
    let stems: Vec<String> = recording.stems.iter().map(|stem| stem.name.clone()).collect();
    if recording.mix.frames() == 0 {
        return Heard { stems, grid: Grid { beats_per_bar: 4, ..Grid::default() }, ..Heard::default() };
    }
    let mix = recording.mix.mono();
    let hits = recording.hits();
    let pitched = recording.pitched();

    // The beats, each moved onto the hit it belongs to.
    let (tracked, tracker) = match &options.beats {
        Some(found) if found.beats.len() >= 4 => ((found.beats.clone(), found.downbeats.clone()), Tracker::Model),
        _ => (beats::track(&hits, &pitched), Tracker::Onsets),
    };
    let mut grid = Grid::build(&tracked.0, &tracked.1, seconds, None, tracker);
    if !grid.beats.is_empty() {
        grid.beats = onsets::snap(&grid.beats, &onsets::onsets(&hits, 0.07))
            .iter()
            .map(|time| (time * 1e5).round() / 1e5)
            .collect();
        grid = grid.measured();
    }
    for &correction in &options.corrections {
        grid = grid.corrected(correction);
    }

    let silent = recording.mix.is_silent();
    let bass_stem = recording.stem("bass").map(Sound::mono);
    let harmony = (!silent).then(|| harmony::analyse(&pitched, bass_stem.as_deref(), &grid.beats));

    // What follows from the stems and the grid: parts, loops, single hits.
    let bar_times = grid.bar_times(seconds);
    let per_bar = grid.beats_per_bar.max(1);
    let stem_signals: Vec<(String, Vec<f32>)> =
        recording.stems.iter().map(|stem| (stem.name.clone(), stem.sound.mono())).collect();
    let whole = structure::bar_features(&mix, &bar_times, per_bar);
    let stem_features: Vec<(String, structure::BarFeatures)> = stem_signals
        .iter()
        .map(|(name, signal)| (name.clone(), structure::bar_features(signal, &bar_times, per_bar)))
        .collect();
    let named: Vec<(String, &structure::BarFeatures)> =
        stem_features.iter().map(|(name, features)| (name.clone(), features)).collect();
    let parts = structure::parts(&whole, &named, &bar_times);

    // Besides the single stems, loops are looked for in everything
    // together, in everything but the vocals, and in everything but the
    // vocals and the drums: where leaving those out leaves something else.
    let blend = |without: &[&str]| -> Option<(Vec<String>, structure::BarFeatures)> {
        let members: Vec<&(String, Vec<f32>)> =
            stem_signals.iter().filter(|(name, _)| !without.contains(&name.as_str())).collect();
        if members.len() < 2 || members.len() == stem_signals.len() {
            return None;
        }
        let mut sum = vec![0.0f32; mix.len()];
        for (_, signal) in &members {
            for (slot, sample) in sum.iter_mut().zip(signal) {
                *slot += sample;
            }
        }
        Some((
            members.iter().map(|(name, _)| name.clone()).collect(),
            structure::bar_features(&sum, &bar_times, per_bar),
        ))
    };
    let instrumental = blend(&["vocals"]);
    let melodic = blend(&["vocals", "drums"]);
    let mut sources: Vec<loops::Source<'_>> = stem_features
        .iter()
        .map(|(name, features)| loops::Source {
            name: name.clone(),
            label: stem_label(name),
            stems: vec![name.clone()],
            single: true,
            features,
        })
        .collect();
    sources.push(loops::Source {
        name: loops::MIX.to_owned(),
        label: "Full mix".to_owned(),
        stems: stems.clone(),
        single: false,
        features: &whole,
    });
    if let Some((members, features)) = &instrumental {
        sources.push(loops::Source {
            name: "instrumental".to_owned(),
            label: "Instrumental".to_owned(),
            stems: members.clone(),
            single: false,
            features,
        });
    }
    if let Some((members, features)) = &melodic {
        sources.push(loops::Source {
            name: "melodic".to_owned(),
            label: "No drums".to_owned(),
            stems: members.clone(),
            single: false,
            features,
        });
    }
    let found_loops = loops::find(&sources, &whole, &stems, &bar_times, per_bar, &parts);

    // One sound stands in for each stem it was not taken apart into, with
    // the bass looked for where a bass is: under everything else.
    let flats = harmony.as_ref().is_some_and(|harmony| harmony.key.key.uses_flats());
    let drums = recording.stem("drums").map(Sound::mono).or_else(|| stems.is_empty().then(|| mix.clone()));
    let bass = bass_stem.or_else(|| stems.is_empty().then(|| hits::band(&mix, None, Some(260.0))));
    let voice = recording.stem("vocals").map(Sound::mono).or_else(|| stems.is_empty().then(|| mix.clone()));
    let mut one_shots = Vec::new();
    let mut groove = Vec::new();
    if let Some(drums) = drums.as_deref().filter(|_| !silent) {
        one_shots.extend(hits::pick_drums(&hits::drum_hits(drums)));
        groove = hits::groove(drums);
    }
    let mut bass_line = Vec::new();
    if let Some(bass) = bass.as_deref().filter(|_| !silent) {
        let followed = pitch::track(bass, 30.0, 400.0);
        one_shots.extend(hits::bass_notes(bass, flats, Some(&followed), 12));
        bass_line = pitch::notes(&followed, 0.09);
    }
    let melody = voice
        .as_deref()
        .filter(|_| !silent)
        .map(|voice| pitch::notes(&pitch::track(voice, 70.0, 1100.0), 0.07))
        .unwrap_or_default();

    Heard {
        seconds: (seconds * 1e4).round() / 1e4,
        activity: stem_signals.iter().map(|(name, signal)| (name.clone(), structure::activity(signal))).collect(),
        stems,
        grid,
        harmony,
        parts,
        loops: found_loops,
        one_shots,
        groove,
        bass: bass_line,
        melody,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::testing::{Noise, drum, hertz, silence, tone};

    /// Eight bars at 100 beats a minute in A minor: drums, a bass that
    /// walks through Am, Dm, E and Am, and a voice over it.
    fn band() -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let beat = 0.6;
        let seconds = 0.3 + 32.0 * beat + 0.6;
        let (mut drums, mut bass, mut voice) = (silence(seconds), silence(seconds), silence(seconds));
        let mut noise = Noise(77);
        for bar in 0..8 {
            let start = 0.3 + (bar * 4) as f64 * beat;
            for step in 0..8 {
                let at = start + step as f64 * beat / 2.0;
                drum(&mut drums, at, "hat", 0.3, &mut noise);
                match step {
                    0 | 4 => drum(&mut drums, at, "kick", 0.9, &mut noise),
                    2 | 6 => drum(&mut drums, at, "snare", 0.7, &mut noise),
                    _ => {}
                }
            }
            let (root, third, fifth) =
                [(33.0, 36.0, 40.0), (38.0, 41.0, 45.0), (28.0, 32.0, 35.0), (33.0, 36.0, 40.0)][bar % 4];
            for (index, note) in [root, root, fifth, third].into_iter().enumerate() {
                tone(&mut bass, start + index as f64 * beat, beat * 0.9, hertz(note, 0.0), 0.5);
            }
            // The voice holds the chord's third and fifth, two beats each.
            tone(&mut voice, start, beat * 1.9, hertz(third + 24.0, 0.0), 0.3);
            tone(&mut voice, start + 2.0 * beat, beat * 1.9, hertz(fifth + 24.0, 0.0), 0.3);
        }
        (drums, bass, voice)
    }

    fn stems() -> Recording {
        let (drums, bass, voice) = band();
        Recording::of_stems(vec![
            Stem { name: "vocals".into(), sound: Sound::from_mono(voice) },
            Stem { name: "drums".into(), sound: Sound::from_mono(drums) },
            Stem { name: "bass".into(), sound: Sound::from_mono(bass) },
        ])
    }

    #[test]
    fn a_recording_in_stems_is_heard_whole() {
        let heard = hear(&stems(), &Options::default());
        assert_eq!(heard.stems, ["vocals", "drums", "bass"]);
        assert!((heard.grid.bpm - 100.0).abs() < 0.7, "{}", heard.grid.bpm);
        assert_eq!(heard.grid.tracker, Tracker::Onsets);
        // The beats sit on the hits: a pre-roll before each.
        let on_hit = heard
            .grid
            .beats
            .iter()
            .filter(|&&time| {
                ((time + onsets::PRE_ROLL - 0.3) / 0.6)
                    .fract()
                    .min(1.0 - ((time + onsets::PRE_ROLL - 0.3) / 0.6).fract().abs())
                    < 0.02
            })
            .count();
        assert!(on_hit >= 30, "{on_hit} of {} beats are on a hit", heard.grid.beats.len());

        let harmony = heard.harmony.as_ref().unwrap();
        assert_eq!(heard.key().map(|reading| reading.key.name()).as_deref(), Some("Am"));
        let names: Vec<String> = harmony.chords.iter().map(|chord| chord.name(heard.flats())).collect();
        assert_eq!(names[..3], ["Am", "Dm", "E"], "{names:?}");

        // The groove: sixteen kicks, sixteen snares, a hat on every eighth.
        let count = |kind: HitKind| heard.groove.iter().filter(|note| note.kind == kind).count();
        assert_eq!((count(HitKind::Kick), count(HitKind::Snare)), (16, 16));
        assert!(count(HitKind::Hat) >= 56, "{}", count(HitKind::Hat));
        // The bass line and the melody, note for note.
        assert_eq!(heard.bass.len(), 32, "{:?}", heard.bass.iter().map(|note| note.midi).collect::<Vec<_>>());
        assert_eq!(heard.bass[..4].iter().map(|note| note.midi).collect::<Vec<_>>(), [33, 33, 40, 36]);
        assert_eq!(heard.melody.len(), 16);
        assert_eq!(heard.melody[..2].iter().map(|note| note.midi).collect::<Vec<_>>(), [60, 64]);
        // One-shots: drums by kind, and a bass note for each pitch played.
        assert!(heard.one_shots.iter().any(|shot| shot.kind == HitKind::Kick));
        assert!(heard.one_shots.iter().any(|shot| shot.label == "Bass A1"));
        // Loops in every stem and in the blends, each said to be where it is.
        assert!(heard.loops.iter().any(|found| found.source == "drums" && !found.solo));
        assert!(heard.loops.iter().any(|found| found.source == "mix"));
        assert_eq!(heard.activity.len(), 3);
    }

    #[test]
    fn one_sound_is_heard_without_being_taken_apart() {
        let (drums, bass, voice) = band();
        let mix: Vec<f32> = (0..drums.len()).map(|i| drums[i] + bass[i] + voice[i]).collect();
        let heard = hear(&Recording::whole(Sound::from_mono(mix)), &Options::default());
        assert!(heard.stems.is_empty());
        assert!((heard.grid.bpm - 100.0).abs() < 0.7, "{}", heard.grid.bpm);
        assert_eq!(heard.key().map(|reading| reading.key.name()).as_deref(), Some("Am"));
        let chords = &heard.harmony.as_ref().unwrap().chords;
        assert_eq!(chords[..3].iter().map(|chord| chord.name(false)).collect::<Vec<_>>(), ["Am", "Dm", "E"]);
        // Every kick is found. But a groove read from a whole mix is a
        // sketch: under 110 Hz a bass note is a hit like a kick, and what
        // lands with it in the middle is taken for the kick's beater.
        let kicks: Vec<f64> =
            heard.groove.iter().filter(|note| note.kind == HitKind::Kick).map(|note| note.time).collect();
        for index in 0..16 {
            let at = 0.3 + index as f64 * 1.2;
            assert!(kicks.iter().any(|time| (time - at).abs() < 0.015), "no kick at {at}");
        }
        // And the bass is found under the rest.
        assert!(heard.bass.len() >= 24, "{}", heard.bass.len());
        assert!(heard.loops.iter().all(|found| found.source == "mix"));
    }

    #[test]
    fn beats_from_a_model_are_taken_and_corrections_are_made_in_order() {
        let recording = stems();
        let beats: Vec<f64> = (0..32).map(|index| 0.3 + index as f64 * 0.6).collect();
        let downbeats: Vec<f64> = beats.iter().copied().step_by(4).collect();
        let options = Options { beats: Some(TrackedBeats { beats, downbeats }), corrections: Vec::new() };
        let heard = hear(&recording, &options);
        assert_eq!(heard.grid.tracker, Tracker::Model);
        assert!((heard.grid.bpm - 100.0).abs() < 0.2);
        assert!((heard.grid.beats[heard.grid.bars[0]] - (0.3 - onsets::PRE_ROLL)).abs() < 0.003);

        let half =
            hear(&recording, &Options { corrections: vec![Correction::Half, Correction::Rotate], ..options.clone() });
        assert!((half.grid.bpm - 50.0).abs() < 0.2);
        assert_eq!(half.grid.bars[0], heard.grid.bars[0] / 2 + 1);
        // Chords are read over the corrected beats: two beats to a chord now.
        assert!(half.harmony.unwrap().chords.len() >= 4);
    }

    #[test]
    fn what_was_heard_is_written_as_midi_on_its_own_beats() {
        let heard = hear(&stems(), &Options::default());
        let first_bar = heard.grid.beats[heard.grid.bars[0]];
        let bass = heard.midi(MidiPart::Bass, first_bar, heard.seconds, None);
        assert_eq!(bass.tracks[0].name, "Bass line");
        assert!((bass.tempo.unwrap() - 100.0).abs() < 0.7);
        let starts: Vec<f64> = bass.tracks[0].notes.iter().take(4).map(|note| note.start).collect();
        for (start, beat) in starts.iter().zip([0.0, 1.0, 2.0, 3.0]) {
            assert!((start - beat).abs() < 0.08, "{starts:?}");
        }
        let groove = heard.midi(MidiPart::Groove, first_bar, heard.seconds, None);
        assert!(groove.tracks[0].drums);
        // On sixteenths, every one of them.
        assert!(groove.tracks[0].notes.iter().all(|note| (note.start * 4.0).fract() == 0.0));
        let chords = heard.midi(MidiPart::Chords, first_bar, heard.seconds, None);
        assert_eq!(chords.tracks[0].notes[..4].iter().map(|note| note.pitch).collect::<Vec<_>>(), [45, 57, 60, 64]);
        assert!(!trill_core::midi_file::write(&heard.midi(MidiPart::Melody, 0.0, heard.seconds, Some(2))).is_empty());
    }

    #[test]
    fn silence_and_nothing_at_all_are_heard_as_nothing() {
        let heard = hear(&Recording::whole(Sound::from_mono(silence(2.0))), &Options::default());
        assert!(heard.grid.beats.is_empty() && heard.loops.is_empty() && heard.one_shots.is_empty());
        assert!(heard.groove.is_empty() && heard.bass.is_empty() && heard.melody.is_empty());
        assert!(heard.key().is_none());
        let nothing = hear(&Recording::default(), &Options::default());
        assert_eq!(nothing.seconds, 0.0);
        assert!(nothing.midi(MidiPart::Groove, 0.0, 1.0, None).tracks[0].notes.is_empty());
    }

    #[test]
    fn the_same_audio_is_heard_the_same_and_survives_being_written_down() {
        let recording = stems();
        let (first, second) = (hear(&recording, &Options::default()), hear(&recording, &Options::default()));
        assert_eq!(first, second);
        let text = serde_json::to_string(&first).unwrap();
        let back: Heard = serde_json::from_str(&text).unwrap();
        assert_eq!(back.grid, first.grid);
        assert_eq!(back.loops, first.loops);
        assert_eq!(stem_label("piano"), "Keys");
        assert_eq!(stem_label("strings"), "Strings");
    }
}
