//! Writes the wavetable synth's bank for someone to listen to:
//!
//! ```sh
//! cargo run -p trill-app --example wavetable_listen -- target/wavetable-listening
//! ```
//!
//! A file for each kind of sound (basses, leads, plucks, pads, keys, and
//! the rest), in which every sound of the kind plays a short phrase that
//! suits it, one after another, through the effects it brings: what a
//! track plays that is given the sound. Four short pieces in different
//! styles, made of the bank and the built-in drum kit and of nothing
//! else. And `LIST.md`, which says when each sound begins in its file.
//!
//! The first sixteen sounds were heard by the owner. Nobody has heard the
//! rest, nor the pieces: whoever wrote them cannot. It is the same on
//! every run and on every machine.
//!
//! With a second path it also writes the small song the pictures of the
//! synth's panel are made from (`render.sh wavetable`).
//!
//! `wavetable_listen --sound-pictures <DIR>` writes nothing to listen to:
//! it makes what the pictures of a sound of more than one device are
//! taken of (`render.sh instrument-sound`), a song and a profile folder
//! that holds one saved sound.

use std::fmt::Write as _;
use std::path::Path;
use trill_core::audio::AudioBuffer;
use trill_core::device::{DeviceFactory, Preset};
use trill_core::presets::{PresetRef, names_after};
use trill_core::project::{AutomationPoint, AutomationTarget};
use trill_core::{ChainRef, DeviceId, Note, Project, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::render::{RenderOptions, render};
use trill_media::{WavFormat, write_wav};

const SYNTH: &str = "builtin:wavetable";
const DRUMS: &str = "builtin:drumkit";
const RATE: u32 = 48_000;

fn n(pitch: u8, start: f64, length: f64, velocity: u8) -> Note {
    Note { pitch, start, length, velocity }
}

fn line(notes: &[(u8, f64, f64, u8)]) -> Vec<Note> {
    notes.iter().map(|(pitch, start, length, velocity)| n(*pitch, *start, *length, *velocity)).collect()
}

/// The presets of a built-in device.
fn presets_of(uri: &str) -> Vec<Preset> {
    let descriptors = BuiltinFactory.descriptors();
    descriptors.into_iter().find(|descriptor| descriptor.uri == uri).expect("a built-in device").presets
}

fn preset(uri: &str, name: &str) -> Preset {
    presets_of(uri).into_iter().find(|preset| preset.name == name).unwrap_or_else(|| panic!("no '{name}' of {uri}"))
}

/// Adds a track on the bank's sound `sound`, with the effects it brings
/// behind the synth: what a track is that is given the sound.
fn track_on(project: &mut Project, name: &str, sound: &str) -> (TrackId, DeviceId) {
    let preset = preset(SYNTH, sound);
    let track = project.add_track(name);
    let device = project.add_device(ChainRef::Track(track), SYNTH, "Wavetable", None).expect("a device");
    let model = project.device_mut(device).expect("the device");
    model.params = preset.params.iter().cloned().collect();
    model.state = preset.state.clone();
    let reference = PresetRef { id: preset.name.clone(), name: preset.name.clone() };
    model.preset = Some(reference.clone());
    project.bring_after(device, &reference, preset.after.as_deref().unwrap_or_default()).expect("its effects");
    (track, device)
}

/// Puts `notes` on `track`, in a clip from `start` that is `beats` long.
fn play(project: &mut Project, track: TrackId, start: f64, beats: f64, notes: &[Note]) {
    let clip = project.add_midi_clip(track, "Part", start, beats).expect("a clip");
    project.clip_mut(clip).expect("the clip").add_notes(notes).expect("notes");
}

fn draw(project: &mut Project, device: DeviceId, param: &str, points: &[(f64, f64)]) {
    let target = AutomationTarget::DeviceParam { device, param: param.to_string() };
    let points = points.iter().map(|(beat, value)| AutomationPoint::new(*beat, *value)).collect();
    project.set_automation(target, points).expect("automation");
}

fn rendered(project: &Project, tail: f64) -> anyhow::Result<(Vec<f32>, Vec<f32>)> {
    let options = RenderOptions { sample_rate: RATE, max_tail_seconds: tail, ..RenderOptions::default() };
    let out = render(project, &BuiltinFactory, &options).map_err(|error| anyhow::anyhow!("{error}"))?;
    anyhow::ensure!(out.problems.is_empty(), "{:?}", out.problems);
    Ok((out.left, out.right))
}

/// The highest a file goes: one that would go higher is turned down to
/// it as a whole, since a file cannot hold more than full scale.
const CEILING: f32 = 0.891;

/// Writes `left` and `right` to `path`. Returns the length in seconds and
/// the peak as it was rendered, in dBFS, as words: with by how much the
/// whole file was turned down, if it had to be.
fn write(mut left: Vec<f32>, mut right: Vec<f32>, path: &Path) -> anyhow::Result<(f64, String)> {
    let peak = left.iter().chain(&right).fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    let seconds = left.len() as f64 / f64::from(RATE);
    let db = |gain: f32| 20.0 * f64::from(gain.max(1e-9)).log10();
    let mut said = format!("{:.1} dBFS", db(peak));
    if peak > CEILING {
        let down = CEILING / peak;
        for sample in left.iter_mut().chain(right.iter_mut()) {
            *sample *= down;
        }
        said = format!("{said} as rendered; the whole file is {:.1} dB down from that, to fit", -db(down));
    }
    let buffer = AudioBuffer { sample_rate: f64::from(RATE), left, right };
    write_wav(path, &buffer, WavFormat::Int24).map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok((seconds, said))
}

/// Triads and sevenths by name, as the notes of one voicing.
fn chord(name: &str) -> &'static [u8] {
    match name {
        "Em" => &[52, 55, 59, 64],
        "C" => &[48, 52, 55, 60, 64],
        "G" => &[43, 50, 55, 59],
        "D" => &[50, 54, 57, 62],
        "Am" => &[45, 52, 57, 60],
        "Dm" => &[50, 53, 57, 62],
        "F" => &[41, 48, 53, 57],
        "Fm" => &[53, 56, 60, 65],
        "Ab" => &[56, 60, 63, 68],
        "Bbm" => &[58, 61, 65, 70],
        "Em9" => &[52, 59, 62, 67, 66],
        "Cmaj7" => &[48, 55, 59, 64],
        "Dm9" => &[50, 57, 60, 64, 65],
        "G13" => &[43, 53, 59, 64],
        "Cmaj9" => &[48, 55, 59, 62, 64],
        "Am9" => &[45, 55, 59, 60, 64],
        _ => &[60, 64, 67],
    }
}

/// `names` held for `each` beats one after another, every note of a chord
/// at `velocity` (a little softer the higher it is).
fn held(names: &[&str], each: f64, velocity: u8) -> Vec<Note> {
    names
        .iter()
        .enumerate()
        .flat_map(|(index, name)| {
            let start = index as f64 * each;
            chord(name)
                .iter()
                .enumerate()
                .map(move |(voice, pitch)| n(*pitch, start, each - 0.1, velocity.saturating_sub(voice as u8 * 3)))
        })
        .collect()
}

/// Every chord of `names` struck short at each of `at`, the beats of a
/// stretch of `each` beats to the chord.
fn stabs(names: &[&str], each: f64, at: &[f64], length: f64, velocity: u8) -> Vec<Note> {
    let mut notes = Vec::new();
    for (index, name) in names.iter().enumerate() {
        for offset in at {
            notes.extend(chord(name).iter().map(|pitch| n(*pitch, index as f64 * each + offset, length, velocity)));
        }
    }
    notes
}

/// An arpeggio up and down each chord of `names`, `steps` notes of
/// `length` beats to the chord, `octave` semitones up.
fn arpeggio(names: &[&str], steps: usize, length: f64, octave: u8) -> Vec<Note> {
    let mut notes = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let tones = chord(name);
        let up_and_down: Vec<u8> =
            tones.iter().chain(tones.iter().rev().skip(1).take(tones.len().saturating_sub(2))).copied().collect();
        for step in 0..steps {
            let pitch = up_and_down[step % up_and_down.len()] + octave;
            let velocity = [112, 78, 92, 70][step % 4] + if step % 8 == 0 { 8 } else { 0 };
            notes.push(n(pitch, index as f64 * steps as f64 * length + step as f64 * length, length * 0.9, velocity));
        }
    }
    notes
}

/// `notes` again from `after` beats on, `times` times in all.
fn repeated(notes: &[Note], after: f64, times: usize) -> Vec<Note> {
    (0..times)
        .flat_map(|time| notes.iter().map(move |note| Note { start: note.start + time as f64 * after, ..*note }))
        .collect()
}

// ---- every sound on a phrase that suits it -----------------------------------

/// What a sound plays to be listened to, at 120 BPM: its notes, how many
/// beats they take, and what they are in words.
struct Suited {
    notes: Vec<Note>,
    beats: f64,
    plays: &'static str,
}

fn suiting(sound: &Preset) -> Suited {
    let of = |notes: Vec<Note>, beats: f64, plays: &'static str| Suited { notes, beats, plays };
    let name = sound.name.as_str();
    match (sound.category.as_str(), name) {
        ("Bass", "808 Sub" | "Deep Sub") => of(
            line(&[
                (36, 0.0, 1.6, 110),
                (36, 2.0, 0.6, 105),
                (39, 2.5, 1.6, 105),
                (34, 4.5, 1.6, 110),
                (36, 6.5, 1.4, 110),
            ]),
            8.0,
            "long low notes, one of them sliding into the next",
        ),
        (
            "Bass",
            "Reese" | "Neuro Bass" | "Wobble Bass" | "Growl Bass" | "Vowel Bass" | "Metal Bass" | "Pulse Bass"
            | "Saw Bass",
        ) => of(line(&[(40, 0.0, 4.05, 105), (43, 4.0, 2.05, 105), (38, 6.0, 1.9, 105)]), 8.0, "long notes: E, G, D"),
        ("Bass", "Rumble") => {
            let notes =
                (0..8).flat_map(|beat| [0.25, 0.5, 0.75].map(|at| n(28, f64::from(beat) + at, 0.2, 100))).collect();
            of(notes, 8.0, "sixteenths on a low E between the beats, where a kick would be")
        }
        ("Bass", "Acid Line") => {
            let pitches = [36, 36, 48, 36, 39, 36, 36, 46, 43, 36, 48, 36, 36, 39, 41, 36];
            let notes = (0..32usize)
                .map(|step| {
                    let (accent, slide) = ([2, 7, 10].contains(&(step % 16)), [7, 14].contains(&(step % 16)));
                    let length = if slide { 0.32 } else { 0.2 };
                    n(pitches[step % 16], step as f64 * 0.25, length, if accent { 122 } else { 84 })
                })
                .collect();
            of(notes, 8.0, "sixteenths with accents and slides")
        }
        ("Bass", _) => of(
            line(&[
                (40, 0.0, 0.45, 110),
                (40, 0.5, 0.2, 80),
                (43, 1.0, 0.45, 100),
                (40, 1.5, 0.2, 85),
                (45, 2.0, 0.45, 110),
                (40, 2.5, 0.2, 80),
                (47, 3.0, 0.45, 105),
                (50, 3.5, 0.4, 95),
                (40, 4.0, 0.9, 115),
                (52, 5.0, 0.2, 90),
                (50, 5.5, 0.2, 85),
                (47, 6.0, 0.45, 100),
                (45, 6.5, 0.45, 95),
                (43, 7.0, 0.9, 105),
            ]),
            8.0,
            "a bass line in E minor",
        ),
        ("Lead", _) => of(
            line(&[
                (72, 0.0, 1.05, 105),
                (74, 1.0, 0.55, 95),
                (76, 1.5, 1.05, 105),
                (79, 2.5, 0.55, 100),
                (76, 3.0, 1.05, 100),
                (74, 4.0, 0.55, 95),
                (72, 4.5, 0.55, 95),
                (71, 5.0, 1.05, 100),
                (67, 6.0, 1.9, 110),
            ]),
            8.0,
            "a melody whose notes overlap, so that a sound that slides does",
        ),
        ("Pluck", "House Stab") => {
            let keys = [57u8, 62, 64, 57];
            let notes = keys
                .iter()
                .enumerate()
                .flat_map(|(bar, key)| [0.5, 1.5].map(|at| n(*key, bar as f64 * 2.0 + at, 0.25, 105)));
            of(notes.collect(), 8.0, "one key at a time, off the beat: A, D, E, A")
        }
        ("Pluck", "Dub Chord") => of(
            line(&[(57, 0.5, 0.3, 100), (57, 3.0, 0.3, 90), (62, 4.5, 0.3, 100), (55, 7.0, 0.3, 90)]),
            8.0,
            "one key at a time, far apart",
        ),
        ("Pluck", _) => of(arpeggio(&["Am", "F", "C", "G"], 8, 0.25, 12), 8.0, "sixteenth arpeggios over Am, F, C, G"),
        ("Pad", _) => of(held(&["Am9", "Cmaj7"], 4.0, 85), 8.0, "Am9, then Cmaj7, two bars in all"),
        ("Keys", "House Organ") => {
            of(stabs(&["Am", "F", "C", "G"], 2.0, &[0.5, 1.5], 0.22, 100), 8.0, "chords off the beat: Am, F, C, G")
        }
        ("Keys", "Clav") => {
            let on =
                [true, false, true, true, false, true, false, true, true, false, true, false, true, true, false, true];
            let notes = (0..32usize)
                .filter(|step| on[step % 16])
                .map(|step| {
                    n([52, 64, 59, 62][step % 4], step as f64 * 0.25, 0.15, if step % 4 == 0 { 115 } else { 90 })
                })
                .collect();
            of(notes, 8.0, "sixteenth comping on E minor")
        }
        ("Keys", _) => {
            let mut notes = held(&["Dm9", "G13"], 2.0, 88);
            notes.extend(chord("Cmaj9").iter().map(|pitch| n(*pitch, 4.0, 3.8, 84)));
            notes.extend(line(&[(76, 4.5, 0.45, 95), (79, 5.0, 0.45, 100), (81, 5.5, 0.9, 105)]));
            of(notes, 8.0, "Dm9, G13, Cmaj9, with three notes on top")
        }
        ("Bell", "Marimba" | "Kalimba") => {
            of(arpeggio(&["Am", "F", "C", "G"], 4, 0.5, 12), 8.0, "eighths over Am, F, C, G")
        }
        ("Bell", _) => of(
            line(&[
                (72, 0.0, 0.9, 100),
                (76, 1.0, 0.9, 95),
                (79, 2.0, 0.9, 105),
                (84, 3.0, 0.9, 100),
                (83, 4.0, 0.9, 95),
                (79, 5.0, 0.9, 100),
                (76, 6.0, 1.9, 105),
            ]),
            8.0,
            "a melody in quarter notes",
        ),
        ("Strings", "Staccato Strings") => {
            of(stabs(&["Am", "F", "C", "G"], 2.0, &[0.0, 0.5, 1.0, 1.5], 0.22, 100), 8.0, "eighths over Am, F, C, G")
        }
        ("Strings", _) => of(held(&["Am", "F"], 4.0, 85), 8.0, "Am, then F, two bars in all"),
        ("Brass", "Brass Stab") => {
            of(stabs(&["F", "G"], 4.0, &[0.0, 0.75, 1.5, 2.5, 3.5], 0.2, 108), 8.0, "stabs on F, then on G")
        }
        ("Brass", "Synth Brass") => {
            let mut notes = stabs(&["F"], 2.0, &[0.0, 1.0], 0.4, 105);
            notes.extend(chord("G").iter().map(|pitch| n(*pitch + 12, 2.0, 1.9, 105)));
            notes.extend(chord("C").iter().map(|pitch| n(*pitch, 4.0, 3.5, 100)));
            of(notes, 8.0, "two stabs, then held chords")
        }
        ("Brass", _) => of(held(&["F", "C"], 4.0, 85), 8.0, "F, then C, two bars in all"),
        ("Texture", "Drone") => {
            of(line(&[(36, 0.0, 15.5, 90), (43, 4.0, 11.5, 90)]), 16.0, "a low C, joined by the G over it")
        }
        ("Texture", "Wind") => of(line(&[(60, 0.0, 11.5, 90)]), 12.0, "one key held"),
        ("Texture", "Swarm") => of(
            line(&[(48, 0.0, 11.5, 90), (55, 0.0, 11.5, 90), (61, 4.0, 7.5, 90)]),
            12.0,
            "C and G, joined by a C sharp",
        ),
        ("Texture", "Shimmer") => of(held(&["Am9"], 12.0, 85), 12.0, "Am9 held for three bars"),
        ("Texture", _) => of(held(&["Am", "F"], 4.0, 85), 8.0, "Am, then F, two bars in all"),
        ("FX", "Riser" | "Pitch Riser") => {
            of(line(&[(48, 0.0, 16.0, 100)]), 16.0, "one key held for four bars and cut off")
        }
        ("FX", "Fall" | "Sweep") => of(line(&[(48, 0.0, 8.0, 100)]), 8.0, "one key held for two bars"),
        ("FX", "Impact") => of(line(&[(36, 0.0, 1.0, 110)]), 4.0, "one low key, once"),
        ("FX", "Sub Drop") => of(line(&[(36, 0.0, 4.0, 110)]), 6.0, "one key, once"),
        ("FX", "Siren") => of(line(&[(72, 0.0, 4.0, 100)]), 4.0, "one key held for a bar"),
        _ => of(
            line(&[(60, 0.0, 0.2, 110), (60, 1.0, 0.2, 110), (72, 1.5, 0.2, 110), (48, 2.5, 0.2, 110)]),
            4.0,
            "four short notes",
        ),
    }
}

/// A sound's own range, read from the end of what it says of itself:
/// `... Range E1 to G2`. The whole keyboard for one that does not say.
fn range_of(sound: &Preset) -> (u8, u8) {
    let key = |name: &str| -> Option<u8> {
        let name = name.trim().trim_end_matches('.');
        let split = name.find(|c: char| c == '-' || c.is_ascii_digit())?;
        let (note, octave) = name.split_at(split);
        let names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
        let semitone = names.iter().position(|n| *n == note)?;
        let octave: i32 = octave.parse().ok()?;
        u8::try_from((octave + 1) * 12 + semitone as i32).ok()
    };
    let Some((_, said)) = sound.description.rsplit_once("Range ") else { return (0, 127) };
    let mut ends = said.split(" to ");
    match (ends.next().and_then(key), ends.next().and_then(key)) {
        (Some(low), Some(high)) => (low, high),
        _ => (0, 127),
    }
}

/// One sound on its phrase, through its effects, with nothing on the
/// master: an octave up or down from where the phrase is written, if that
/// is what brings it into the keys the sound is for.
fn alone(sound: &Preset) -> (Project, Suited) {
    let mut suited = suiting(sound);
    let (low, high) = range_of(sound);
    let lowest = suited.notes.iter().map(|note| note.pitch).min().unwrap_or(60);
    let highest = suited.notes.iter().map(|note| note.pitch).max().unwrap_or(60);
    let shift: i16 = if lowest < low {
        12
    } else if highest > high {
        -12
    } else {
        0
    };
    for note in &mut suited.notes {
        note.pitch = (i16::from(note.pitch) + shift).clamp(0, 127) as u8;
    }
    let mut project = Project::new(&sound.name);
    project.set_tempo(120.0).expect("a tempo");
    let (track, _) = track_on(&mut project, &sound.name, &sound.name);
    play(&mut project, track, 0.0, suited.beats, &suited.notes);
    (project, suited)
}

/// Every sound of `kinds`, one after another with a breath between, into
/// `file`; and when each begins, into `list`.
fn kinds_file(folder: &Path, file: &str, kinds: &[&str], list: &mut String) -> anyhow::Result<()> {
    let gap = (0.6 * f64::from(RATE)) as usize;
    let (mut left, mut right): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
    let mut rows = String::new();
    for sound in presets_of(SYNTH).iter().filter(|sound| kinds.contains(&sound.category.as_str())) {
        let (project, suited) = alone(sound);
        let (l, r) = rendered(&project, 5.0)?;
        let at = left.len() as f64 / f64::from(RATE);
        let with = match names_after(sound.after.as_deref().unwrap_or_default()) {
            names if names.is_empty() => "dry".to_string(),
            names => format!("through its {}", names.join(" and ").to_lowercase()),
        };
        writeln!(
            rows,
            "| {}:{:04.1} | {} | {} | {}, {with} |",
            at as u64 / 60,
            at % 60.0,
            sound.name,
            sound.category,
            suited.plays
        )?;
        left.extend(l);
        right.extend(r);
        left.extend(std::iter::repeat_n(0.0, gap));
        right.extend(std::iter::repeat_n(0.0, gap));
        println!("{file}: {}", sound.name);
    }
    let (seconds, peak) = write(left, right, &folder.join(file))?;
    writeln!(
        list,
        "\n### `{file}`\n\n{} minutes {:02} seconds; its peak is {peak}.\n\n\
         | Begins at | Sound | Kind | What it plays |\n|---|---|---|---|\n{rows}",
        seconds as u64 / 60,
        seconds as u64 % 60
    )?;
    Ok(())
}

// ---- four pieces ---------------------------------------------------------------

/// A song that is being put together of the bank's sounds and the
/// built-in drum kit.
struct Song {
    project: Project,
}

impl Song {
    fn new(name: &str, tempo: f64) -> Self {
        let mut project = Project::new(name);
        project.set_tempo(tempo).expect("a tempo");
        // As a new song has it: a limiter on the master.
        project.add_device(ChainRef::Master, "builtin:limiter", "Limiter", None).expect("a limiter");
        Self { project }
    }

    /// A part on one of the bank's sounds, from the beat `start`, with its
    /// track's fader at `fader` dB.
    fn part(&mut self, name: &str, sound: &str, fader: f32, start: f64, beats: f64, notes: &[Note]) -> DeviceId {
        let (track, device) = track_on(&mut self.project, name, sound);
        self.project.set_track_volume(track, fader).expect("a fader");
        play(&mut self.project, track, start, beats, notes);
        device
    }

    /// The drum kit on one of its presets.
    fn drums(&mut self, kit: &str, fader: f32, beats: f64, notes: &[Note]) {
        let track = self.project.add_track("Drums");
        let device = self.project.add_device(ChainRef::Track(track), DRUMS, "Drum Kit", None).expect("a drum kit");
        self.project.device_mut(device).expect("the kit").params = preset(DRUMS, kit).params.into_iter().collect();
        self.project.set_track_volume(track, fader).expect("a fader");
        play(&mut self.project, track, 0.0, beats, notes);
    }
}

const KICK: u8 = 36;
const SNARE: u8 = 38;
const CLAP: u8 = 39;
const HAT: u8 = 42;
const OPEN_HAT: u8 = 46;
const CRASH: u8 = 49;

/// Sixteen bars of house at 124: a stab, a round bass off the beat, a
/// pad, a marimba, and a riser into the second half.
fn house(kit: &str) -> Project {
    let mut song = Song::new("House", 124.0);
    let bars = 16.0 * 4.0;
    // A minor, D minor, E minor, A minor: a bar each.
    let (roots, stab_keys) = ([33u8, 38, 40, 33], [57u8, 62, 64, 57]);
    let stab: Vec<Note> = (0..16usize)
        .flat_map(|bar| [0.75, 1.5, 2.75, 3.5].map(|at| n(stab_keys[bar % 4], bar as f64 * 4.0 + at, 0.25, 104)))
        .collect();
    let stabs = song.part("Stab", "House Stab", -3.0, 0.0, bars, &stab);
    draw(&mut song.project, stabs, "macro2", &[(0.0, 0.0), (16.0, 0.1), (32.0, 0.7), (32.01, 0.35), (64.0, 0.5)]);

    let mut beat = Vec::new();
    for bar in 0..16 {
        let start = f64::from(bar) * 4.0;
        // The kick drops out for the last bar before the second half.
        if bar != 7 {
            beat.extend((0..4).map(|quarter| n(KICK, start + f64::from(quarter), 0.25, 118)));
        }
        beat.extend((0..4).map(|quarter| n(HAT, start + f64::from(quarter) + 0.5, 0.2, 92)));
        if bar >= 4 {
            beat.extend([1.0, 3.0].map(|at| n(CLAP, start + at, 0.25, 106)));
            beat.push(n(OPEN_HAT, start + 3.5, 0.4, 80));
        }
    }
    beat.push(n(CRASH, 32.0, 1.0, 100));
    song.drums(kit, -8.0, bars, &beat);

    let bass: Vec<Note> = (4..16usize)
        .flat_map(|bar| {
            let (root, start) = (roots[bar % 4], bar as f64 * 4.0);
            [(0.5, 0), (1.5, 0), (2.5, 0), (3.5, 12)].map(|(at, up)| n(root + up, start + at - 16.0, 0.4, 108))
        })
        .collect();
    song.part("Bass", "House Bass", -3.5, 16.0, 48.0, &bass);
    let pad: Vec<Note> = held(&["Am", "Dm", "Em", "Am"], 4.0, 78);
    song.part("Pad", "Warm Pad", -13.0, 16.0, 48.0, &repeated(&pad, 16.0, 3));
    let hook = line(&[
        (69, 0.0, 0.4, 100),
        (72, 0.75, 0.4, 92),
        (76, 1.5, 0.4, 104),
        (74, 2.5, 0.4, 92),
        (72, 3.0, 0.4, 96),
        (69, 4.0, 0.4, 100),
        (74, 4.75, 0.4, 92),
        (77, 5.5, 0.4, 104),
        (76, 6.5, 0.9, 96),
    ]);
    song.part("Marimba", "Marimba", -7.0, 32.0, 32.0, &repeated(&hook, 8.0, 4));
    song.part("Riser", "Riser", -18.0, 16.0, 16.0, &[n(48, 0.0, 16.0, 100)]);
    song.part("Impact", "Impact", -9.0, 32.0, 8.0, &[n(36, 0.0, 1.0, 110)]);
    song.project
}

/// Sixteen bars of drum and bass at 174: a dark pad and a bell, then a
/// sub drop, a reese under a two-step beat, and a sine lead.
fn drum_and_bass(kit: &str) -> Project {
    let mut song = Song::new("Drum and Bass", 174.0);
    let bars = 16.0 * 4.0;
    // F minor, A flat, B flat minor, F minor: four bars each.
    let chords = held(&["Fm", "Ab", "Bbm", "Fm"], 16.0, 76);
    song.part("Pad", "Dark Pad", -12.0, 0.0, bars, &chords);
    let bell =
        line(&[(72, 0.0, 1.5, 96), (75, 2.0, 1.5, 90), (80, 4.0, 1.5, 100), (79, 6.0, 1.5, 90), (77, 8.0, 3.5, 96)]);
    song.part("Bell", "Glass Bell", -9.0, 0.0, 32.0, &repeated(&bell, 16.0, 2));

    let mut beat = Vec::new();
    for bar in 0..16 {
        let start = f64::from(bar) * 4.0;
        beat.extend(
            (0..8).map(|eighth| n(HAT, start + f64::from(eighth) * 0.5, 0.15, if eighth % 2 == 0 { 96 } else { 70 })),
        );
        if bar >= 4 {
            beat.extend([n(KICK, start, 0.25, 120), n(KICK, start + 2.5, 0.25, 112)]);
            beat.extend([n(SNARE, start + 1.0, 0.25, 116), n(SNARE, start + 3.0, 0.25, 118)]);
            if bar % 4 == 3 {
                beat.extend([n(SNARE, start + 3.5, 0.2, 84), n(SNARE, start + 3.75, 0.2, 96)]);
            }
        }
    }
    beat.push(n(CRASH, 16.0, 1.0, 100));
    song.drums(kit, -5.0, bars, &beat);
    song.part("Sub Drop", "Sub Drop", -14.0, 16.0, 8.0, &[n(41, 0.0, 4.0, 110)]);

    // The reese: long notes that slide where they meet.
    let roots = [29u8, 32, 34, 29];
    let low: Vec<Note> = (0..6usize)
        .map(|half| {
            let root = roots[(half / 2 + 1) % 4];
            n(if half % 2 == 1 { root + 3 } else { root }, half as f64 * 8.0, 8.05, 106)
        })
        .collect();
    let reese = song.part("Reese", "Reese", -5.0, 16.0, 48.0, &low);
    draw(&mut song.project, reese, "macro1", &[(16.0, 0.0), (48.0, 0.15), (64.0, 0.7)]);
    let tune = line(&[
        (77, 0.0, 2.05, 100),
        (80, 2.0, 1.05, 96),
        (84, 3.0, 3.05, 104),
        (82, 6.0, 1.05, 96),
        (80, 7.0, 1.05, 96),
        (79, 8.0, 3.05, 100),
        (77, 11.0, 4.0, 104),
    ]);
    song.part("Lead", "Sine Lead", -12.0, 32.0, 32.0, &repeated(&tune, 16.0, 2));
    song.part("Fall", "Fall", -20.0, 48.0, 8.0, &[n(48, 0.0, 8.0, 100)]);
    song.project
}

/// Sixteen bars of synthwave at 100: a bass in running eighths, an
/// arpeggio, brass chords, and a square lead over the second half.
fn synthwave(kit: &str) -> Project {
    let mut song = Song::new("Synthwave", 100.0);
    let bars = 16.0 * 4.0;
    let names = ["Em", "C", "G", "D"];
    let roots = [28u8, 36, 31, 38];
    let bass: Vec<Note> = (0..16usize)
        .flat_map(|bar| {
            let root = roots[bar % 4];
            (0..8).map(move |eighth| {
                n(if eighth % 2 == 1 { root + 12 } else { root }, bar as f64 * 4.0 + f64::from(eighth) * 0.5, 0.4, 104)
            })
        })
        .collect();
    song.part("Bass", "Pluck Bass", -5.0, 0.0, bars, &bass);
    let arp = arpeggio(&names, 16, 0.25, 12);
    let arps = song.part("Arp", "Arp Pluck", -8.5, 0.0, bars, &repeated(&arp, 16.0, 4));
    draw(&mut song.project, arps, "macro2", &[(0.0, 0.0), (32.0, 0.5), (64.0, 0.3)]);

    let mut beat = Vec::new();
    for bar in 0..16 {
        let start = f64::from(bar) * 4.0;
        beat.extend([n(KICK, start, 0.25, 116), n(KICK, start + 2.0, 0.25, 112)]);
        beat.extend(
            (0..8).map(|eighth| n(HAT, start + f64::from(eighth) * 0.5, 0.15, if eighth % 2 == 0 { 90 } else { 68 })),
        );
        if bar >= 4 {
            beat.extend([n(SNARE, start + 1.0, 0.3, 116), n(SNARE, start + 3.0, 0.3, 118)]);
        }
    }
    beat.push(n(CRASH, 32.0, 1.0, 100));
    song.drums(kit, -4.0, bars, &beat);
    let brass = held(&names, 4.0, 88);
    song.part("Brass", "Synth Brass", -8.5, 16.0, 48.0, &repeated(&brass, 16.0, 3));
    song.part("Sweep", "Sweep", -21.0, 24.0, 8.0, &[n(48, 0.0, 8.0, 100)]);
    let tune = line(&[
        (76, 0.0, 1.5, 104),
        (79, 1.5, 0.5, 96),
        (83, 2.0, 2.0, 108),
        (84, 4.0, 1.0, 100),
        (83, 5.0, 1.0, 100),
        (79, 6.0, 2.0, 104),
        (78, 8.0, 1.5, 100),
        (79, 9.5, 0.5, 96),
        (83, 10.0, 2.0, 108),
        (81, 12.0, 1.0, 100),
        (78, 13.0, 1.0, 100),
        (74, 14.0, 2.0, 104),
    ]);
    song.part("Lead", "Square Lead", -12.5, 32.0, 32.0, &repeated(&tune, 16.0, 2));
    song.project
}

/// Twelve bars of lo-fi at 80: dusty keys, an electric bass, a lazy beat,
/// a tape lead and a thumb piano.
fn lofi(kit: &str) -> Project {
    let mut song = Song::new("Lo-fi", 80.0);
    let bars = 12.0 * 4.0;
    let names = ["Dm9", "G13", "Cmaj9", "Am9"];
    let mut comping = Vec::new();
    for bar in 0..12usize {
        let (name, start) = (names[bar % 4], bar as f64 * 4.0);
        comping.extend(chord(name).iter().map(|pitch| n(*pitch, start, 2.3, 76)));
        comping.extend(chord(name).iter().skip(1).map(|pitch| n(*pitch, start + 2.58, 1.2, 62)));
    }
    song.part("Keys", "Dusty Keys", 0.0, 0.0, bars, &comping);
    let roots = [38u8, 31, 36, 33];
    let bass: Vec<Note> = (0..12usize)
        .flat_map(|bar| {
            let (root, start) = (roots[bar % 4], bar as f64 * 4.0);
            [n(root, start, 1.4, 104), n(root, start + 2.58, 0.7, 92), n(root + 7, start + 3.5, 0.4, 88)]
        })
        .collect();
    song.part("Bass", "Electric Bass", -5.0, 0.0, bars, &bass);

    // A lazy beat: the off-beat hats come late.
    let mut beat = Vec::new();
    for bar in 0..12 {
        let start = f64::from(bar) * 4.0;
        beat.extend([n(KICK, start, 0.25, 110), n(KICK, start + 2.58, 0.25, 100)]);
        beat.extend([n(SNARE, start + 1.0, 0.3, 104), n(SNARE, start + 3.0, 0.3, 106)]);
        beat.extend((0..4).flat_map(|quarter| {
            [n(HAT, start + f64::from(quarter), 0.15, 84), n(HAT, start + f64::from(quarter) + 0.58, 0.15, 60)]
        }));
    }
    song.drums(kit, -5.0, bars, &beat);
    let tune = line(&[
        (69, 0.5, 0.5, 92),
        (72, 1.0, 0.5, 86),
        (76, 1.5, 1.0, 100),
        (74, 2.58, 0.5, 86),
        (72, 3.0, 1.0, 92),
        (71, 4.5, 0.5, 90),
        (74, 5.0, 0.5, 86),
        (77, 5.5, 1.0, 100),
        (76, 6.58, 0.5, 86),
        (74, 7.0, 1.0, 92),
        (72, 8.5, 0.5, 90),
        (76, 9.0, 0.5, 92),
        (79, 9.5, 1.5, 104),
        (76, 11.0, 1.0, 88),
        (72, 12.5, 0.5, 88),
        (71, 13.0, 0.5, 86),
        (69, 13.5, 2.0, 96),
    ]);
    song.part("Lead", "Tape Lead", -10.5, 16.0, 32.0, &repeated(&tune, 16.0, 2));
    let thumb = arpeggio(&names, 8, 0.5, 24);
    song.part("Kalimba", "Kalimba", -15.0, 32.0, 16.0, &thumb);
    song.project
}

/// The drum kit's preset whose name has one of `wanted` in it, in that
/// order of liking; its first, if it has none of them.
fn kit(wanted: &[&str]) -> String {
    let kits = presets_of(DRUMS);
    let found = wanted.iter().find_map(|word| kits.iter().find(|kit| kit.name.to_lowercase().contains(word)));
    found.or(kits.first()).map(|kit| kit.name.clone()).expect("the drum kit has a preset")
}

/// A piece's mix in numbers: how loud it is, what the limiter on the
/// master took off, and each part's loudness as it leaves its fader.
fn mixed(measured: &trill_engine::render::Measured) -> String {
    let mut said = format!("the mix is at {:.1} LUFS", measured.stats.loudness_lufs);
    if let Some(limiter) = measured.dynamics.iter().find(|device| device.track.is_none()) {
        write!(said, ", and the limiter takes off {:.1} dB at the most", limiter.most_db).expect("a string");
    }
    let parts: Vec<String> =
        measured.tracks.iter().map(|track| format!("{} {:.1}", track.name, track.loudness_lufs)).collect();
    format!("{said}. Its parts, in LUFS: {}.", parts.join(", "))
}

// ---- pictures ------------------------------------------------------------------

/// The song the pictures of the panel are made from: one sound with a
/// table of its own on the second oscillator, playing a few notes.
fn for_pictures() -> Project {
    let mut project = Project::new("Wavetable");
    project.set_tempo(120.0).expect("a tempo");
    let lead = preset(SYNTH, "Sync Lead");
    let track = project.add_track("Sync Lead");
    let device = project.add_device(ChainRef::Track(track), SYNTH, "Sync Lead", None).expect("a device");
    play(&mut project, track, 0.0, 8.0, &suiting(&lead).notes);
    let model = project.device_mut(device).expect("the synth");
    model.params = lead.params.iter().cloned().collect();
    let mut state = trill_dsp::wavetable::State::from_device_state(lead.state.as_deref()).unwrap_or_default();
    let recipe = r#"{ "name": "Thin Reed", "size": 32,
        "frames": [ { "shape": "pulse", "width": 0.5 }, { "shape": "pulse", "width": 0.04, "tilt": -3 } ] }"#;
    state.osc2 = Some(trill_dsp::wavetable::TableSource::Recipe(serde_json::from_str(recipe).expect("a recipe")));
    model.state = Some(state.to_bytes());
    model.params.insert("osc2_table".into(), 0.0);
    model.params.insert("osc2_position".into(), 0.35);
    project
}

/// What the pictures of a sound of more than one device are taken of, in
/// `folder`: `profile`, which holds one sound that was saved from a track
/// (a pad of the poly synth in a long hall), and `sound.trill`, a song with
/// a track on that sound and a track on one the wavetable synth brings.
/// Made through a session, as the window and a model make them.
fn sound_pictures(folder: &Path) -> anyhow::Result<()> {
    use trill_engine::{AudioBackend, Session};
    let failed = |error: trill_engine::SessionError| anyhow::anyhow!("{error}");
    let profile = folder.join("profile");
    if folder.exists() {
        std::fs::remove_dir_all(folder)?;
    }
    std::fs::create_dir_all(&profile)?;
    let factory = std::sync::Arc::new(BuiltinFactory);
    let mut session = Session::new(factory, AudioBackend::Null { sample_rate: RATE }).map_err(failed)?;
    session.saved_presets().open(profile.join("saved-presets"));

    let keys = session.edit(|project| Ok(project.add_track("Warm Hall"))).map_err(failed)?;
    session.add_device(ChainRef::Track(keys), "builtin:polysynth", Some("warm_pad"), None).map_err(failed)?;
    let hall = session.add_device(ChainRef::Track(keys), "builtin:reverb", None, None).map_err(failed)?;
    session.set_device_params(hall, &[("decay".into(), 6.0), ("mix".into(), 0.35)]).map_err(failed)?;
    session.save_sound(keys, "Warm Hall", "A soft pad in a long hall", &["pad".into()]).map_err(failed)?;
    let pad = session.edit(|project| Ok(project.add_track("Glass Pad"))).map_err(failed)?;
    session.add_device(ChainRef::Track(pad), SYNTH, Some("Glass Pad"), None).map_err(failed)?;
    let chords = held(&["Em9", "Cmaj7", "Em9", "Cmaj7"], 4.0, 80);
    session
        .edit(|project| {
            for (track, name) in [(keys, "Warm Hall"), (pad, "Glass Pad")] {
                let clip = project.add_midi_clip(track, name, 0.0, 16.0)?;
                project.clip_mut(clip)?.add_notes(&chords)?;
            }
            Ok(())
        })
        .map_err(failed)?;
    let song = folder.join("sound.trill");
    session.save(&song).map_err(failed)?;
    println!("{}", song.display());
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args().skip(1);
    let Some(folder) = arguments.next() else {
        anyhow::bail!("usage: wavetable_listen <DIR> [<PROJECT.trill> for the pictures], or --sound-pictures <DIR>");
    };
    if folder == "--sound-pictures" {
        let Some(folder) = arguments.next() else { anyhow::bail!("--sound-pictures needs a folder") };
        return sound_pictures(Path::new(&folder));
    }
    let folder = Path::new(&folder);
    std::fs::create_dir_all(folder)?;

    let bank = presets_of(SYNTH);
    let sounds = bank.iter().filter(|sound| !sound.category.is_empty()).count();
    let mut list = format!(
        "# The wavetable synth's bank, to listen to\n\n\
         {sounds} sounds. The first sixteen (Solid Bass, Reese, Acid Line, Growl Bass, Deep Sub, Sync Lead, Anthem \
         Saws, Talk Lead, Glass Pad, PWM Strings, Choir, Glass Pluck, String Pluck, Trance Pluck, Tine Piano, FM Keys) \
         were heard and passed; their levels have moved by a few decibels since, so that each kind is at one level \
         through its effects. **Nobody has heard the others, nor the four pieces**: they were written by someone who \
         cannot hear, from how such sounds are made, and are held to what can be measured (level, room for a chord, \
         every key, every macro, and where the sounds of a synth that is known to be good lie).\n\n\
         Made by `cargo run -p trill-app --example wavetable_listen -- <this folder>`; the same on every run.\n\n\
         ## A file for each kind\n\n\
         Every sound plays a short phrase that suits it at 120 BPM, through the effects it brings and with nothing \
         else after it: what a track plays that is given the sound. The sounds of a file are at the levels they \
         have against one another.\n"
    );
    kinds_file(folder, "1 Basses.wav", &["Bass"], &mut list)?;
    kinds_file(folder, "2 Leads.wav", &["Lead"], &mut list)?;
    kinds_file(folder, "3 Plucks.wav", &["Pluck"], &mut list)?;
    kinds_file(folder, "4 Pads.wav", &["Pad"], &mut list)?;
    kinds_file(folder, "5 Keys.wav", &["Keys"], &mut list)?;
    kinds_file(folder, "6 The rest.wav", &["Bell", "Strings", "Brass", "Texture", "FX"], &mut list)?;

    let (house_kit, tight, plain, dusty) =
        (kit(&["909", "house"]), kit(&["tight", "punch", "909"]), kit(&["default"]), kit(&["lo", "soft"]));
    list.push_str(
        "\n## Four pieces\n\n\
         Each is made of sounds of the bank, each with the effects it brings, and of the built-in drum kit: track \
         faders, the limiter a new song has on its master, and nothing else.\n\n\
         | File | What it is | Seconds | Peak |\n|---|---|---|---|\n",
    );
    let pieces: [(&str, Project, String); 4] = [
        (
            "Piece 1 House.wav",
            house(&house_kit),
            format!(
                "124 BPM, sixteen bars in A minor. House Stab and the drum kit ({house_kit}) begin; House Bass, Warm \
                 Pad and a Riser come in at bar 5; an Impact and a Marimba at bar 9"
            ),
        ),
        (
            "Piece 2 Drum and Bass.wav",
            drum_and_bass(&tight),
            format!(
                "174 BPM, sixteen bars in F minor. Dark Pad and Glass Bell begin; a Sub Drop, a Reese and the beat \
                 (the drum kit's {tight}) come in at bar 5; Sine Lead has the tune from bar 9, and a Fall sounds at \
                 bar 13"
            ),
        ),
        (
            "Piece 3 Synthwave.wav",
            synthwave(&plain),
            format!(
                "100 BPM, sixteen bars in E minor. Pluck Bass, Arp Pluck and the drum kit ({plain}) begin; Synth \
                 Brass holds the chords from bar 5; a Sweep leads into bar 9, where Square Lead takes the tune"
            ),
        ),
        (
            "Piece 4 Lo-fi.wav",
            lofi(&dusty),
            format!(
                "80 BPM, twelve bars. Dusty Keys, Electric Bass and the drum kit ({dusty}) begin; Tape Lead has the \
                 tune from bar 5; a Kalimba joins for the last four bars"
            ),
        ),
    ];
    let mut mixes = String::new();
    for (file, project, what) in pieces {
        let options = RenderOptions { sample_rate: RATE, max_tail_seconds: 6.0, ..RenderOptions::default() };
        let out = render(&project, &BuiltinFactory, &options).map_err(|error| anyhow::anyhow!("{error}"))?;
        anyhow::ensure!(out.problems.is_empty(), "{:?}", out.problems);
        writeln!(mixes, "- `{file}`: {}", mixed(&out.measured))?;
        let (seconds, peak) = write(out.left, out.right, &folder.join(file))?;
        writeln!(list, "| `{file}` | {what} | {seconds:.0} | {peak} |")?;
        println!("{file}");
    }
    write!(
        list,
        "
How each is mixed, as measured, since nobody has heard it (loudness over the whole piece, so a part that          plays for a few bars reads lower than it sounds while it plays):

{mixes}"
    )?;
    list.push_str("\n## What each sound says of itself\n\n");
    for sound in bank.iter().filter(|sound| !sound.category.is_empty()) {
        writeln!(list, "- **{}** ({}): {}", sound.name, sound.category, sound.description)?;
    }
    std::fs::write(folder.join("LIST.md"), list)?;

    if let Some(project) = arguments.next() {
        std::fs::write(&project, for_pictures().to_json())?;
        println!("{project}");
    }
    Ok(())
}
