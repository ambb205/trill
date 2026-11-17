//! Writes a small sample pack and a project that uses it, for screenshots
//! and for trying the sound browser without a library of one's own:
//!
//! ```sh
//! cargo run -p trill-app --example sound_fixtures -- crates/trill-app/screenshots/sounds
//! ```
//!
//! Everything is synthesized here from sines and noise, the same on every
//! run, so the pictures made from it are the same on every machine and
//! nothing in them is anybody's property. The pack goes into `<DIR>/Night
//! Shift` and the project into `<DIR>/night-shift.trill`. The project
//! names its files by the path given, so it opens from wherever that path
//! leads to them: for the screenshots, the repository root.
//!
//! A second project, `<DIR>/night-shift-moved.trill`, is the same song as
//! it would be had the pack been moved since: it names every file in
//! `<DIR>/Sample Packs/Night Shift`, where there is nothing. That is what
//! the pictures of the missing-files list are made from.
//!
//! The pack also holds two sampled instruments, each an SFZ file with its
//! recordings in a folder of its own (`Night Shift/Instruments`): keys
//! with three layers of loudness, a second way of playing and a sound for
//! a key coming up, and a small drum kit with two takes of every hit. A
//! third project, `<DIR>/night-keys.trill`, is the song with a track
//! playing through each: what the pictures of the Sampled Instrument are
//! made from.

use std::f64::consts::TAU;
use std::path::{Path, PathBuf};
use trill_core::audio::AudioBuffer;
use trill_core::project::{AudioContent, WarpMode};
use trill_core::{ChainRef, FolderMove, Note, Project, Relink};
use trill_dsp::{SampleFiles, drumrack, sampler};
use trill_media::{WavFormat, write_wav};

const RATE: f64 = 44_100.0;

/// Deterministic noise in -1..1.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(self.0 >> 8) / f64::from(1u32 << 23) - 1.0
    }
}

fn frames(seconds: f64) -> usize {
    (seconds * RATE).round() as usize
}

/// `seconds` of sound, each sample from its time in seconds.
fn render(seconds: f64, mut sample: impl FnMut(f64) -> f64) -> Vec<f32> {
    (0..frames(seconds)).map(|frame| sample(frame as f64 / RATE) as f32).collect()
}

/// An exponential decay that has fallen to a thousandth after `seconds`.
fn decay(time: f64, seconds: f64) -> f64 {
    (-6.9 * time / seconds).exp()
}

fn kick(low: f64, seconds: f64) -> Vec<f32> {
    let mut phase = 0.0;
    render(seconds, |time| {
        let frequency = low + 110.0 * decay(time, 0.09);
        phase += frequency / RATE;
        (phase * TAU).sin() * decay(time, seconds) * 0.9
    })
}

fn snare(tone: f64, seconds: f64, seed: u32) -> Vec<f32> {
    let mut noise = Noise(seed);
    render(seconds, |time| {
        let body = (time * tone * TAU).sin() * decay(time, 0.1) * 0.5;
        (body + noise.next() * decay(time, seconds) * 0.55) * 0.9
    })
}

/// Noise with its lows taken out: hats and cymbals.
fn metal(seconds: f64, seed: u32, level: f64) -> Vec<f32> {
    let mut noise = Noise(seed);
    let mut previous = 0.0;
    render(seconds, |time| {
        let white = noise.next();
        let bright = white - previous;
        previous = white;
        bright * decay(time, seconds) * level
    })
}

fn clap(seed: u32) -> Vec<f32> {
    let mut noise = Noise(seed);
    render(0.3, |time| {
        let bursts: f64 =
            [0.0, 0.012, 0.024].iter().filter(|start| time >= **start).map(|start| decay(time - start, 0.02)).sum();
        noise.next() * (bursts * 0.5 + decay(time, 0.3) * 0.4) * 0.6
    })
}

fn tom(frequency: f64) -> Vec<f32> {
    render(0.4, |time| (time * (frequency + 40.0 * decay(time, 0.1)) * TAU).sin() * decay(time, 0.4) * 0.8)
}

/// A note of `frequency` with a few harmonics, plucked.
fn pluck(frequency: f64, seconds: f64) -> Vec<f32> {
    render(seconds, |time| {
        let harmonics: f64 = (1..=5)
            .map(|n| (time * frequency * f64::from(n) * TAU).sin() / f64::from(n) * decay(time, seconds / f64::from(n)))
            .sum();
        harmonics * 0.45
    })
}

/// Puts `hit` into `mix` starting at `at` seconds.
fn place(mix: &mut [f32], hit: &[f32], at: f64, level: f32) {
    let start = frames(at);
    for (slot, sample) in mix.iter_mut().skip(start).zip(hit) {
        *slot += sample * level;
    }
}

/// Two bars of drums at `tempo`.
fn drum_loop(tempo: f64, busy: bool) -> Vec<f32> {
    let beat = 60.0 / tempo;
    let mut mix = vec![0.0; frames(8.0 * beat)];
    let (kick, snare, hat, open) = (kick(48.0, 0.3), snare(190.0, 0.2, 7), metal(0.05, 11, 0.5), metal(0.25, 13, 0.4));
    for bar in 0..2 {
        let start = f64::from(bar) * 4.0 * beat;
        for (at, level) in [(0.0, 1.0), (1.5, 0.7), (2.0, 1.0), (if busy { 3.25 } else { 2.5 }, 0.8)] {
            place(&mut mix, &kick, start + at * beat, level);
        }
        for at in [1.0, 3.0] {
            place(&mut mix, &snare, start + at * beat, 0.9);
        }
        for step in 0..if busy { 16 } else { 8 } {
            let at = f64::from(step) * if busy { 0.25 } else { 0.5 };
            place(&mut mix, &hat, start + at * beat, if step % 2 == 0 { 0.6 } else { 0.35 });
        }
        place(&mut mix, &open, start + 3.5 * beat, 0.5);
    }
    mix
}

/// Two bars of a bass line at `tempo`: `notes` are (beat, length, hertz).
fn line(tempo: f64, beats: f64, notes: &[(f64, f64, f64)], voice: impl Fn(f64, f64) -> Vec<f32>) -> Vec<f32> {
    let beat = 60.0 / tempo;
    let mut mix = vec![0.0; frames(beats * beat)];
    for (at, length, frequency) in notes {
        place(&mut mix, &voice(*frequency, length * beat), at * beat, 0.8);
    }
    mix
}

/// A sustained chord that swells and falls: a pad.
fn pad(seconds: f64, chord: &[f64]) -> Vec<f32> {
    render(seconds, |time| {
        let swell = (time / seconds * std::f64::consts::PI).sin().powf(0.6);
        let voices: f64 = chord
            .iter()
            .map(|frequency| (time * frequency * TAU).sin() + 0.4 * (time * frequency * 2.003 * TAU).sin())
            .sum();
        voices / chord.len() as f64 * swell * 0.45
    })
}

/// A struck bar of glass at `frequency`: `bright` is how hard it was hit,
/// 0..=2, and adds partials.
fn glass(frequency: f64, seconds: f64, bright: u8) -> Vec<f32> {
    let partials: &[(f64, f64)] = match bright {
        0 => &[(1.0, 1.0), (2.0, 0.2)],
        1 => &[(1.0, 1.0), (2.0, 0.4), (2.76, 0.25), (4.0, 0.12)],
        _ => &[(1.0, 1.0), (2.0, 0.55), (2.76, 0.45), (4.0, 0.3), (5.4, 0.22), (7.1, 0.12)],
    };
    let level = [0.3, 0.5, 0.75][usize::from(bright.min(2))];
    render(seconds, |time| {
        let sum: f64 = partials
            .iter()
            .map(|(ratio, gain)| gain * (time * frequency * ratio * TAU).sin() * decay(time, seconds / ratio))
            .sum();
        sum * level * (time / 0.002).min(1.0)
    })
}

/// The hertz of a key: 69 is the A at 440.
fn hertz(key: u8) -> f64 {
    440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0)
}

/// `"C2"`, `"F#4"`: a key as recordings are named.
fn key_name(key: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[usize::from(key % 12)], i32::from(key / 12) - 1)
}

/// Writes the keys: a recording every half octave from C2 to F#5 at three
/// strengths, a muted way of playing chosen by a key, and a sound for a
/// key coming up. Returns the instrument's file as a project names it.
fn glass_keys(pack: &Pack) -> anyhow::Result<String> {
    let folder = "Instruments/Glass Keys";
    let mut text = String::from(
        "// Glass Keys: synthesized for the pictures of Trill.\n<control> default_path=Samples/\n\
         <global> ampeg_release=0.4 sw_lokey=24 sw_hikey=25 sw_default=24\n\n<master> sw_last=24 sw_label=Open\n",
    );
    let roots: Vec<u8> = (0..8).map(|step| 36 + 6 * step).collect();
    let layers = [("soft", 1, 60), ("medium", 61, 100), ("hard", 101, 127)];
    for (bright, (layer, low, high)) in layers.iter().enumerate() {
        text.push_str(&format!("<group> lovel={low} hivel={high}\n"));
        for root in &roots {
            let name = format!("open {layer} {}.wav", key_name(*root));
            pack.write(&format!("{folder}/Samples/{name}"), glass(hertz(*root), 0.9, bright as u8))?;
            let (first, last) = ((*root).saturating_sub(2).max(36), if *root == 78 { 84 } else { root + 3 });
            text.push_str(&format!("<region> sample={name} lokey={first} hikey={last} pitch_keycenter={root}\n"));
        }
    }
    text.push_str("\n<master> sw_last=25 sw_label=Muted\n<group> ampeg_release=0.08\n");
    for root in [42u8, 54, 66, 78] {
        let name = format!("muted {}.wav", key_name(root));
        pack.write(&format!("{folder}/Samples/{name}"), glass(hertz(root), 0.18, 1))?;
        let (first, last) = (root - 6, if root == 78 { 84 } else { root + 5 });
        text.push_str(&format!("<region> sample={name} lokey={first} hikey={last} pitch_keycenter={root}\n"));
    }
    pack.write(&format!("{folder}/Samples/release.wav"), metal(0.07, 53, 0.25))?;
    text.push_str("\n<master> trigger=release\n<region> sample=release.wav lokey=36 hikey=84 pitch_keycenter=60\n");
    std::fs::write(pack.root.join(folder).join("Glass Keys.sfz"), text)?;
    Ok(format!("{}/{folder}/Glass Keys.sfz", pack.named))
}

/// Writes the kit: six drums, each soft and hard, each twice so that a
/// repeated hit is not the same recording. Returns the instrument's file
/// as a project names it.
fn tape_kit(pack: &Pack) -> anyhow::Result<String> {
    let folder = "Instruments/Tape Kit";
    let mut text = String::from(
        "// Tape Kit: synthesized for the pictures of Trill.\n<control> default_path=Samples/\n\
         <global> loop_mode=one_shot\n",
    );
    let drums: [(u8, &str); 6] =
        [(36, "Kick"), (38, "Snare"), (42, "Hihat Closed"), (45, "Tom"), (46, "Hihat Open"), (49, "Crash")];
    for (key, drum) in drums {
        text.push_str(&format!("\n<group> key={key} seq_length=2\n"));
        for (hard, (layer, low, high)) in [("soft", 1, 90), ("hard", 91, 127)].into_iter().enumerate() {
            for take in 1..=2u32 {
                let seed = u32::from(key) * 8 + hard as u32 * 2 + take;
                let level = if hard == 1 { 1.0 } else { 0.55 };
                let hit: Vec<f32> = match key {
                    36 => kick(44.0 + f64::from(take), 0.34),
                    38 => snare(185.0 + 6.0 * f64::from(take), 0.22, seed),
                    42 => metal(0.05, seed, 0.6),
                    45 => tom(118.0 + 3.0 * f64::from(take)),
                    46 => metal(0.36, seed, 0.5),
                    _ => metal(1.4, seed, 0.5),
                };
                let name = format!("{drum} {layer} {take}.wav");
                let hit = hit.into_iter().map(|sample| sample * level).collect();
                pack.write(&format!("{folder}/Samples/{name}"), hit)?;
                text.push_str(&format!("<region> sample={name} lovel={low} hivel={high} seq_position={take}\n"));
            }
        }
    }
    std::fs::write(pack.root.join(folder).join("Tape Kit.sfz"), text)?;
    Ok(format!("{}/{folder}/Tape Kit.sfz", pack.named))
}

struct Pack {
    root: PathBuf,
    /// The root as the project should name it: the path that was given.
    named: String,
}

impl Pack {
    /// Writes `samples` as `name` and returns the path the project uses.
    fn write(&self, name: &str, samples: Vec<f32>) -> anyhow::Result<String> {
        let path = self.root.join(name);
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        // A little apart in the two channels, so the stereo view has two.
        let right: Vec<f32> = samples
            .iter()
            .enumerate()
            .map(|(index, sample)| if index % 2 == 0 { *sample } else { sample * 0.96 })
            .collect();
        write_wav(&path, &AudioBuffer { sample_rate: RATE, left: samples, right }, WavFormat::Int16)?;
        Ok(format!("{}/{name}", self.named))
    }
}

fn main() -> anyhow::Result<()> {
    let Some(folder) = std::env::args().nth(1) else {
        anyhow::bail!("usage: sound_fixtures <DIR>");
    };
    let named = folder.replace('\\', "/").trim_end_matches('/').to_owned();
    let pack = Pack { root: Path::new(&folder).join("Night Shift"), named: format!("{named}/Night Shift") };

    // One-shots.
    let kick_deep = pack.write("Drums/Kicks/Kick Deep 01.wav", kick(45.0, 0.42))?;
    pack.write("Drums/Kicks/Kick Punchy 02.wav", kick(58.0, 0.26))?;
    pack.write("Drums/Kicks/Kick Sub 03.wav", kick(38.0, 0.6))?;
    pack.write("Drums/Kicks/808 Long C.wav", render(1.4, |time| (time * 65.4 * TAU).sin() * decay(time, 1.4) * 0.9))?;
    let snare_tight = pack.write("Drums/Snares/Snare Tight 01.wav", snare(200.0, 0.2, 3))?;
    pack.write("Drums/Snares/Snare Fat 02.wav", snare(165.0, 0.32, 5))?;
    pack.write(
        "Drums/Snares/Rim Shot 01.wav",
        render(0.09, |time| (time * 820.0 * TAU).sin() * decay(time, 0.09) * 0.8),
    )?;
    let clap_wide = pack.write("Drums/Claps/Clap Wide 01.wav", clap(17))?;
    pack.write("Drums/Claps/Clap Short 02.wav", clap(19).into_iter().take(frames(0.16)).collect())?;
    let hat_closed = pack.write("Drums/Hats/HH Closed 01.wav", metal(0.06, 23, 0.6))?;
    pack.write("Drums/Hats/HH Closed 02.wav", metal(0.04, 29, 0.6))?;
    let hat_open = pack.write("Drums/Hats/Hat Open 01.wav", metal(0.38, 31, 0.5))?;
    let tom_mid = pack.write("Drums/Toms/Tom Mid 01.wav", tom(140.0))?;
    pack.write("Drums/Toms/Tom Low 02.wav", tom(96.0))?;
    let crash = pack.write("Drums/Cymbals/Crash Bright 01.wav", metal(1.6, 37, 0.5))?;
    pack.write("Drums/Cymbals/Ride Bell 01.wav", metal(0.9, 41, 0.35))?;
    pack.write("Drums/Perc/Shaker 01.wav", metal(0.12, 43, 0.4))?;
    let pluck_c = pack.write("Keys/Pluck C3.wav", pluck(130.81, 1.2))?;
    pack.write("Keys/Pluck Am Stab.wav", pad(0.6, &[220.0, 261.63, 329.63]))?;
    pack.write("Bass/Sub Bass A1.wav", render(0.9, |time| (time * 55.0 * TAU).sin() * decay(time, 0.9) * 0.9))?;
    let riser = pack.write("FX/Riser Noise 4 Bars.wav", {
        let mut noise = Noise(47);
        render(8.0, move |time| noise.next() * (time / 8.0).powi(2) * 0.5)
    })?;
    pack.write("FX/Impact Low 01.wav", kick(34.0, 1.1))?;
    let vocal = pack.write(
        "Vocals/Vocal Chop Ah 01.wav",
        render(0.45, |time| {
            let voice: f64 = [(1.0, 220.0), (0.6, 660.0), (0.35, 1100.0)]
                .iter()
                .map(|(level, frequency)| level * (time * frequency * TAU).sin())
                .sum();
            voice * (time / 0.05).min(1.0) * decay(time, 0.45) * 0.4
        }),
    )?;

    // Loops, with their tempo (and key) in the name, as packs have them.
    let drums = pack.write("Loops/Drum Loop Steady 120bpm.wav", drum_loop(120.0, false))?;
    let drums_busy = pack.write("Loops/Drum Loop Busy 120bpm.wav", drum_loop(120.0, true))?;
    pack.write("Loops/Drum Loop Half 90bpm.wav", drum_loop(90.0, false))?;
    let bass_notes = [
        (0.0, 1.5, 55.0),
        (1.5, 0.5, 55.0),
        (2.0, 1.0, 65.41),
        (3.0, 1.0, 49.0),
        (4.0, 1.5, 55.0),
        (5.5, 0.5, 82.41),
        (6.0, 2.0, 73.42),
    ];
    let bass = pack.write(
        "Loops/Bass Loop 120bpm Am.wav",
        line(120.0, 8.0, &bass_notes, |frequency, seconds| {
            render(seconds, |time| {
                ((time * frequency * TAU).sin() + 0.3 * (time * frequency * 2.0 * TAU).sin())
                    * decay(time, seconds * 1.6)
                    * 0.7
            })
        }),
    )?;
    let arp_notes: Vec<(f64, f64, f64)> =
        (0..16).map(|step| (f64::from(step) * 0.5, 0.5, [293.66, 349.23, 440.0, 587.33][step as usize % 4])).collect();
    pack.write("Loops/Arp Loop 140bpm Dm.wav", line(140.0, 8.0, &arp_notes, pluck))?;
    let pad_loop = pack.write("Loops/Pad Loop 100bpm F#m.wav", pad(9.6, &[185.0, 220.0, 277.18]))?;
    pack.write("Loops/Chord Loop 90bpm C.wav", pad(10.667, &[130.81, 164.81, 196.0]))?;

    // The project: audio on four tracks, a drum rack and a sampler.
    let mut project = Project::new("Night Shift");
    project.set_tempo(120.0)?;
    let audio = |path: &str, warp: WarpMode, tempo: Option<f64>| AudioContent {
        warp,
        source_tempo: tempo,
        ..AudioContent::new(path)
    };

    let drum_track = project.add_track("Drums");
    project.add_audio_clip(drum_track, "Drum Loop Steady", 0.0, 8.0, audio(&drums, WarpMode::Beats, Some(120.0)))?;
    project.add_audio_clip(drum_track, "Drum Loop Busy", 8.0, 8.0, audio(&drums_busy, WarpMode::Beats, Some(120.0)))?;
    project.add_audio_clip(drum_track, "Drum Loop Steady", 16.0, 8.0, audio(&drums, WarpMode::Beats, Some(120.0)))?;
    project.add_audio_clip(drum_track, "Crash", 24.0, 3.2, audio(&crash, WarpMode::Off, None))?;

    let bass_track = project.add_track("Bass");
    for start in [0.0, 8.0, 16.0] {
        project.add_audio_clip(bass_track, "Bass Loop", start, 8.0, audio(&bass, WarpMode::Tonal, Some(120.0)))?;
    }

    let pad_track = project.add_track("Pad");
    // A hundred beats per minute in a song at a hundred and twenty: its
    // sixteen beats are sixteen beats here too, once it is warped.
    let pad_clip = project.add_audio_clip(
        pad_track,
        "Pad Loop",
        0.0,
        16.0,
        AudioContent { transpose: -2.0, gain_db: -3.0, ..audio(&pad_loop, WarpMode::Tonal, Some(100.0)) },
    )?;
    project.set_clip_fades(pad_clip, 3.0, 4.0)?;
    project.add_audio_clip(
        pad_track,
        "Riser",
        16.0,
        8.0,
        AudioContent { offset_seconds: 4.0, ..audio(&riser, WarpMode::Off, None) },
    )?;

    let vocal_track = project.add_track("Vocal chops");
    for start in [2.0, 3.5, 6.0, 10.0, 11.5, 14.0, 18.0, 19.5] {
        project.add_audio_clip(vocal_track, "Vocal Chop", start, 0.9, audio(&vocal, WarpMode::Off, None))?;
    }
    // A file that is not there: what a moved sample folder looks like.
    project.add_audio_clip(
        vocal_track,
        "Lost Take",
        22.0,
        4.0,
        audio(&format!("{named}/Night Shift/Vocals/Lead Take 03.wav"), WarpMode::Off, None),
    )?;

    let kit_track = project.add_track("Kit");
    let rack = project.add_device(ChainRef::Track(kit_track), "builtin:drumrack", "Drum Rack", None)?;
    let pads = drumrack::State::with_pads([
        (36, kick_deep),
        (38, snare_tight),
        (39, clap_wide),
        (42, hat_closed),
        (45, tom_mid),
        (46, hat_open),
        (49, crash.clone()),
    ])
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    project.device_mut(rack)?.state = Some(pads.to_bytes());
    let beat_clip = project.add_midi_clip(kit_track, "Beat", 0.0, 8.0)?;
    let mut hits = Vec::new();
    for step in 0..32 {
        let start = f64::from(step) * 0.25;
        let mut hit = |pitch: u8, velocity: u8| hits.push(Note { pitch, start, length: 0.25, velocity });
        if step % 8 == 0 || step % 16 == 10 {
            hit(36, 110);
        }
        if step % 8 == 4 {
            hit(38, 100);
        }
        if step % 2 == 0 {
            hit(42, if step % 4 == 0 { 90 } else { 60 });
        }
        if step == 14 || step == 30 {
            hit(46, 80);
        }
    }
    project.clip_mut(beat_clip)?.set_notes(hits)?;
    project.clip_mut(beat_clip)?.set_loop_length(Some(8.0))?;
    project.set_clip_span(beat_clip, 0.0, 24.0)?;

    let keys_track = project.add_track("Keys");
    let keys = project.add_device(ChainRef::Track(keys_track), "builtin:sampler", "Sampler", None)?;
    project.device_mut(keys)?.state = Some(sampler::State::new(pluck_c).to_bytes());
    project.device_mut(keys)?.params.extend([
        ("start".to_owned(), 2.0),
        ("end".to_owned(), 82.0),
        ("release".to_owned(), 0.4),
    ]);
    let riff = project.add_midi_clip(keys_track, "Riff", 8.0, 16.0)?;
    let riff_notes: Vec<Note> =
        [(0.0, 57), (0.5, 60), (1.0, 64), (1.5, 60), (2.0, 55), (2.5, 59), (3.0, 62), (3.5, 59)]
            .into_iter()
            .map(|(start, pitch)| Note { pitch, start, length: 0.45, velocity: 96 })
            .collect();
    project.clip_mut(riff)?.set_notes(riff_notes)?;
    project.clip_mut(riff)?.set_loop_length(Some(4.0))?;
    project.add_device(ChainRef::Master, "builtin:limiter", "Limiter", None)?;

    let file = Path::new(&folder).join("night-shift.trill");
    std::fs::write(&file, project.to_json())?;
    println!("wrote the pack to {} and the project to {}", pack.root.display(), file.display());

    // The song with a track for each of the pack's sampled instruments.
    let mut with_keys = project.clone();
    let plays = |project: &mut Project, name: &str, file: String| -> anyhow::Result<trill_core::TrackId> {
        let track = project.add_track(name);
        let device = project.add_device(ChainRef::Track(track), trill_dsp::sampled::URI, "Sampled Instrument", None)?;
        project.device_mut(device)?.state = Some(trill_dsp::sampled::State::new(file).to_bytes());
        Ok(track)
    };
    let glass_track = plays(&mut with_keys, "Glass", glass_keys(&pack)?)?;
    let glass_line = with_keys.add_midi_clip(glass_track, "Glass line", 0.0, 16.0)?;
    let line: Vec<Note> = [(0.0, 57, 70), (1.0, 64, 96), (2.0, 60, 84), (3.0, 67, 110), (4.0, 55, 66), (5.0, 62, 92)]
        .into_iter()
        .map(|(start, pitch, velocity)| Note { pitch, start, length: 0.9, velocity })
        .collect();
    with_keys.clip_mut(glass_line)?.set_notes(line)?;
    with_keys.clip_mut(glass_line)?.set_loop_length(Some(8.0))?;
    let tape_track = plays(&mut with_keys, "Tape", tape_kit(&pack)?)?;
    let tape_beat = with_keys.add_midi_clip(tape_track, "Tape beat", 0.0, 16.0)?;
    let beat: Vec<Note> = (0..16u8)
        .flat_map(|step| {
            let start = f64::from(step) * 0.5;
            let hat = Note { pitch: 42, start, length: 0.25, velocity: if step % 2 == 0 { 96 } else { 62 } };
            let drum = match step % 8 {
                0 | 5 => Some(Note { pitch: 36, start, length: 0.25, velocity: 112 }),
                2 | 6 => Some(Note { pitch: 38, start, length: 0.25, velocity: 104 }),
                _ => None,
            };
            [Some(hat), drum].into_iter().flatten()
        })
        .collect();
    with_keys.clip_mut(tape_beat)?.set_notes(beat)?;
    let keys_file = Path::new(&folder).join("night-keys.trill");
    std::fs::write(&keys_file, with_keys.to_json())?;
    println!("and the song with its sampled instruments to {}", keys_file.display());

    // The same song as if the pack had been moved since it was saved:
    // every file it names is in a folder where there is nothing.
    let went = FolderMove::new(format!("{named}/Night Shift"), format!("{named}/Sample Packs/Night Shift"));
    let relinks: Vec<Relink> = project
        .files(&SampleFiles)
        .into_iter()
        .filter_map(|file| Some(Relink { place: file.place, path: went.apply(&file.path)? }))
        .collect();
    project.relink(&relinks, &SampleFiles)?;
    let moved = Path::new(&folder).join("night-shift-moved.trill");
    std::fs::write(&moved, project.to_json())?;
    println!("and the song with its pack moved to {}", moved.display());
    Ok(())
}
