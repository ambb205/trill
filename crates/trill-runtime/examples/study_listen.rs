//! Makes a folder to listen to from a study: what only ears can judge of
//! what a study does to a song.
//!
//! ```sh
//! cargo run -p trill-runtime --example study_listen -- <profile folder> <study id> target/study-listening
//! ```
//!
//! `<profile folder>` is one a program was started on with `--profile`
//! (its studies are in `<profile folder>/studies`). Everything is made the
//! way a song is: each thing is taken into a song by the code the window
//! and the tools take it with, and the song is rendered. Nothing is
//! fetched and no model is run: the study is read as it is on disk.
//!
//! The folder holds other people's recording where the study is of one:
//! it belongs under `target/`, and never in the repository.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use trill_core::audio::AudioBuffer;
use trill_core::{Note, Project, TrackId};
use trill_engine::AudioBackend;
use trill_engine::render::{RenderOptions, render};
use trill_listen::{Heard, HitKind, Loop, MidiPart};
use trill_media::{WavFormat, write_wav};
use trill_runtime::taking::{self, Lane, Pitch, TakeNotes, TakeStretch};
use trill_runtime::{Runtime, RuntimeOptions, Storage};

const RATE: u32 = 48_000;
/// The tempos a loop is taken into a song at, beside its own.
const SLOWER: f64 = 110.0;
const FASTER: f64 = 140.0;

struct Made<'a> {
    runtime: &'a Runtime,
    out: &'a Path,
    /// What the list says, a line a file.
    list: String,
}

impl Made<'_> {
    /// Starts a song of nothing at `tempo`.
    fn new_song(&self, tempo: f64) -> Result<(), String> {
        let mut session = self.runtime.session.lock();
        session.new_project();
        // A new song comes with a limiter on its master: what is listened to here is the sound itself.
        let master: Vec<_> = session.project().master.devices.iter().map(|device| device.id).collect();
        session
            .edit(|project| {
                for device in master {
                    project.remove_device(device)?;
                }
                project.set_tempo(tempo)
            })
            .map_err(|error| error.to_string())
    }

    /// The open song from `from` to `to` beats, the tracks named or all of them, as sound.
    fn sound(&self, from: f64, to: f64, only: Option<Vec<TrackId>>) -> Result<AudioBuffer, String> {
        let (project, factory): (Project, _) = {
            let session = self.runtime.session.lock();
            (session.project().clone(), session.factory())
        };
        let options = RenderOptions {
            sample_rate: RATE,
            start_beat: from,
            end_beat: Some(to),
            max_tail_seconds: 1.5,
            only_tracks: only,
            ..RenderOptions::default()
        };
        let rendered = render(&project, &*factory, &options).map_err(|error| error.to_string())?;
        Ok(AudioBuffer { sample_rate: f64::from(RATE), left: rendered.left, right: rendered.right })
    }

    /// Writes `audio` into `name`, and a line of the list for it.
    fn keep(&mut self, name: &str, audio: &AudioBuffer, says: &str) -> Result<(), String> {
        let file = self.out.join(name);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        write_wav(&file, audio, WavFormat::Int24).map_err(|error| error.to_string())?;
        let _ = writeln!(self.list, "- `{name}`: {says}");
        println!("{name}");
        Ok(())
    }

    /// Renders the open song from `from` to `to` beats, the tracks named or all of them, into `name`.
    fn render(&mut self, name: &str, from: f64, to: f64, only: Option<Vec<TrackId>>, says: &str) -> Result<(), String> {
        let audio = self.sound(from, to, only)?;
        self.keep(name, &audio, says)
    }

    fn head(&mut self, title: &str, about: &str) {
        let _ = writeln!(self.list, "\n## {title}\n\n{about}\n");
    }
}

/// How loud a sound is over its whole length, in decibels under full scale.
fn level(audio: &AudioBuffer) -> f64 {
    let frames = audio.left.len().max(1) as f64;
    let power: f64 = audio.left.iter().zip(&audio.right).map(|(l, r)| f64::from(l * l + r * r) / 2.0).sum();
    10.0 * (power / frames).max(1e-12).log10()
}

/// How a stretch was fitted to the song, in words for the list.
fn fitted(plan: &taking::StretchPlan, song: f64) -> String {
    let own = plan.pieces.first().and_then(|piece| piece.source_tempo);
    let mode = match plan.warp {
        trill_core::project::WarpMode::Off => "untouched",
        trill_core::project::WarpMode::Repitch => "repitched",
        trill_core::project::WarpMode::Beats => "stretched in the mode that keeps transients (Beats)",
        trill_core::project::WarpMode::Tonal => "stretched in the mode that keeps sustained sound smooth (Tonal)",
    };
    match own {
        Some(own) => {
            let ratio = own / song;
            let how = if (ratio - 1.0).abs() < 0.002 {
                "at its own length".to_owned()
            } else if ratio > 1.0 {
                format!("{:.0} % longer", (ratio - 1.0) * 100.0)
            } else {
                format!("{:.0} % shorter", (1.0 - ratio) * 100.0)
            };
            let pieces = match plan.pieces.len() {
                1 => String::new(),
                count => format!(", laid in {count} clips because its tempo moves"),
            };
            format!("its own tempo {own:.2}, the song's {song:.2}: {how}, {mode}{pieces}")
        }
        None => "as recorded, untouched".to_owned(),
    }
}

/// The best loop of `source`, one of two bars or more where there is one.
fn best<'a>(heard: &'a Heard, source: &str) -> Option<&'a Loop> {
    let of: Vec<&Loop> = heard.loops.iter().filter(|found| found.source == source).collect();
    of.iter().copied().find(|found| found.bars >= 2).or(of.first().copied())
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let usage = "give the profile folder, the study's id and the folder to write into";
    let (profile, id, out) = (args.next().ok_or(usage)?, args.next().ok_or(usage)?, args.next().ok_or(usage)?);
    let (profile, out) = (PathBuf::from(profile), PathBuf::from(out));
    let storage = Storage::At { settings: profile.join("settings.json"), library_index: None };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: RATE }, plugins: None, storage };
    let runtime = Runtime::start(options).map_err(|error| error.to_string())?;
    let studied = runtime.study(&id)?;
    let heard = studied.heard.clone().ok_or("the study is not done")?;
    let study = studied.report.study.clone();
    let grid = &heard.grid;
    std::fs::create_dir_all(&out).map_err(|error| format!("{}: {error}", out.display()))?;
    let mut made = Made { runtime: &runtime, out: &out, list: String::new() };
    let key = taking::key_of(&heard).map_or_else(|| "no key read".to_owned(), |key| key.name());
    let _ = writeln!(
        made.list,
        "# {}: what a study does to a song, to listen to\n\nThe recording is {:.0} s long, at {:.2} beats a minute, in \
         {key}, {} bars. Every file here was made by taking something of the study into a song, with the code the \
         window and the tools use, and rendering the song at {} Hz. Listen for what only ears can tell: whether a \
         stem is clean, whether a loop comes round without a bump, what the stretch and the pitch shift cost.",
        study.title,
        heard.seconds,
        grid.bpm,
        grid.bars.len(),
        RATE
    );

    // 1. The stems, as the study keeps them.
    made.head("1. The stems alone", "As the study keeps them: the files a clip taken from the study plays.");
    for name in &study.stems {
        let file = studied.folder.stem(name);
        let to = out.join("1 stems").join(format!("{name}.wav"));
        std::fs::create_dir_all(to.parent().unwrap()).map_err(|error| error.to_string())?;
        std::fs::copy(&file, &to).map_err(|error| format!("{}: {error}", file.display()))?;
        let _ = writeln!(made.list, "- `1 stems/{name}.wav`: the {name} stem, the whole recording");
    }

    // 2. Loops, taken into a song at another tempo: two rounds each, so that where it comes round is heard.
    made.head(
        "2. Loops in a song at another tempo",
        &format!(
            "The study's best loop of each kind, as one clip that plays its stretch round, two rounds long: as \
             recorded (untouched, at the recording's own tempo), in a song at {SLOWER:.0} and in a song at \
             {FASTER:.0}. Listen to the seam where the second round begins, and to what the stretch does to the hits \
             and to the held notes against the one that is untouched."
        ),
    );
    let sources = ["drums", "bass", "vocals", "other", "instrumental"];
    for (index, source) in sources.iter().enumerate() {
        let Some(found) = best(&heard, source) else { continue };
        let bars = format!("{} bars from bar {}", found.bars, found.bar + 1);
        for (label, tempo) in [("as recorded", grid.bpm), ("slower", SLOWER), ("faster", FASTER)] {
            made.new_song(tempo)?;
            let mut take = TakeStretch::new(id.clone(), found.stems.clone(), found.start, found.end);
            (take.lane, take.times) = (Lane::New, 2);
            // The one to hold the others against: the recording itself, untouched, at its own tempo.
            take.as_recorded = label == "as recorded";
            let taken = runtime.take_stretch(&take)?;
            let name = format!("2 loops/{} {} - {label} ({tempo:.0}).wav", index + 1, found.label.to_lowercase());
            let says = format!("{} loop, {bars}, twice round; {}", found.label, fitted(&taken.plan, tempo));
            made.render(&name, 0.0, taken.plan.length * 2.0, None, &says)?;
        }
    }

    // 3. A sung phrase moved to another pitch.
    made.head(
        "3. A sung phrase at another pitch",
        "One sung line, from bar line to bar line, on the vocal stem: as recorded, and moved by two semitones and by \
         five, up and down, first at the recording's own tempo (so that only the pitch moves) and then in a song at \
         another tempo as well. The stretcher moves the pitch, and a voice's formants move with it: listen for where \
         the voice stops sounding like itself.",
    );
    let lyrics = studied.lyrics.clone().unwrap_or_default();
    // The longest of the first lines that is sung through: enough of a phrase to judge a voice by.
    let line = lyrics.lines.iter().take(20).max_by(|a, b| a.words.len().cmp(&b.words.len()));
    if let Some(line) = line {
        let (from, to) = taking::whole_bars(&heard, line.start, line.end);
        let bars = format!("bars {} to {}", taking::bar_of(&heard, from + 1e-3), taking::bar_of(&heard, to - 1e-3));
        let moves: [(f64, f64, &str); 7] = [
            (grid.bpm, 0.0, "as recorded"),
            (grid.bpm, 2.0, "2 semitones up"),
            (grid.bpm, 5.0, "5 semitones up"),
            (grid.bpm, -2.0, "2 semitones down"),
            (grid.bpm, -5.0, "5 semitones down"),
            (SLOWER, 2.0, "2 semitones up, slower"),
            (SLOWER, 5.0, "5 semitones up, slower"),
        ];
        for (index, (tempo, semitones, label)) in moves.into_iter().enumerate() {
            made.new_song(tempo)?;
            let mut take = TakeStretch::new(id.clone(), vec!["vocals".to_owned()], from, to);
            take.pitch = if semitones == 0.0 { Pitch::AsRecorded } else { Pitch::Semitones(semitones) };
            take.as_recorded = semitones == 0.0;
            let taken = runtime.take_stretch(&take)?;
            let name = format!("3 phrase/{} {label}.wav", index + 1);
            let says = format!("the line sung in {bars}, {label}; {}", fitted(&taken.plan, tempo));
            made.render(&name, 0.0, taken.plan.length, None, &says)?;
        }
    } else {
        let _ = writeln!(made.list, "This study wrote down no words: there is no phrase to move.");
    }

    // 4. The kit built from its hits, playing a bar.
    made.head(
        "4. The kit built from its hits",
        &format!(
            "The study's cleanest hit of each kind of drum, as one drum rack, playing a plain bar twice at \
             {SLOWER:.0}: kick on one and three, snare on two and four, hats on the eighths, and the rest once each. \
             Listen to each hit: whether it is one drum, and whether anything else of the mix is left in it."
        ),
    );
    made.new_song(SLOWER)?;
    let kit = runtime.take_kit(&id, None)?;
    let has = |note: u8| kit.pads.iter().any(|pad| pad.0 == note);
    let mut notes: Vec<Note> = Vec::new();
    let mut hit = |pitch: u8, start: f64, velocity: u8| {
        if has(pitch) {
            notes.push(Note { pitch, start, length: 0.25, velocity, ..Default::default() });
        }
    };
    for beat in [0.0, 2.0] {
        hit(taking::pad_of(HitKind::Kick), beat, 112);
        hit(taking::pad_of(HitKind::Snare), beat + 1.0, 108);
    }
    for eighth in 0..8 {
        hit(taking::pad_of(HitKind::Hat), f64::from(eighth) * 0.5, if eighth % 2 == 0 { 96 } else { 72 });
    }
    hit(taking::pad_of(HitKind::OpenHat), 3.5, 96);
    hit(taking::pad_of(HitKind::Perc), 1.5, 96);
    hit(taking::pad_of(HitKind::Tom), 3.75, 100);
    hit(taking::pad_of(HitKind::Cymbal), 0.0, 96);
    {
        let mut session = runtime.session.lock();
        let track = kit.track;
        session
            .edit(|project| {
                let clip = project.add_midi_clip(track, "bar", 0.0, 8.0)?;
                let clip = project.clip_mut(clip)?;
                clip.add_notes(&notes)?;
                clip.set_loop_length(Some(4.0))
            })
            .map_err(|error| error.to_string())?;
    }
    let pads: Vec<String> = kit.pads.iter().map(|(note, label, _)| format!("{label} on pad {note}")).collect();
    made.render("4 kit/the kit playing a bar.wav", 0.0, 8.0, None, &format!("the rack's pads: {}", pads.join(", ")))?;
    for (note, label, file) in &kit.pads {
        let to = out.join("4 kit").join(format!("pad {note} {label}.wav"));
        // As 24-bit, which every player reads: the study keeps what it cuts in floating point.
        let hit = trill_media::decode(file).map_err(|error| format!("{}: {error}", file.display()))?;
        write_wav(&to, &hit, WavFormat::Int24).map_err(|error| error.to_string())?;
        let _ =
            writeln!(made.list, "- `4 kit/pad {note} {label}.wav`: that hit by itself, as it was cut from the drums");
    }

    // 5. The bass line and the groove as notes, beside the stems they were read from.
    made.head(
        "5. The bass line and the groove as notes",
        "Eight bars each, in a song that has taken the recording's tempo, so that the stem lies on the song's bars \
         untouched: the notes alone on one of Trill's own instruments, the stem alone, and the two together, all six decibels down so that the two together have room. Listen \
         to whether the notes are the ones the stem plays, and whether they are where it plays them.",
    );
    let parts = [
        (MidiPart::Bass, "bass", "bass", "builtin:polysynth", "the poly synth"),
        (MidiPart::Groove, "drums", "groove", "builtin:drumkit", "the drum kit"),
    ];
    for (part, stem, word, instrument, instrument_name) in parts {
        // Eight bars from the first two-bar loop of that stem: where it plays what it plays most.
        let first = best(&heard, stem).map_or(16, |found| found.bar) as i64;
        let (from, to) = (grid.bar_start(first), grid.bar_start(first + 8));
        made.new_song(grid.bpm)?;
        // A mastered stem reaches full scale by itself: the song six decibels down, so that the two together
        // have room, and all three files of a part at the same level.
        runtime.session.lock().edit(|project| project.set_master_volume(-6.0)).map_err(|error| error.to_string())?;
        runtime.take_tempo(&id, 0.0)?;
        let at = grid.bar_beat(first) - grid.bar_beat(0);
        let take = TakeNotes {
            study: id.clone(),
            part,
            from,
            to,
            lane: Lane::New,
            at,
            quantize: None,
            instrument: Some(instrument.to_owned()),
        };
        let written = runtime.take_notes(&take, None)?;
        let mut beside = TakeStretch::new(id.clone(), vec![stem.to_owned()], from, to);
        (beside.at, beside.as_recorded) = (at, true);
        let stretch = runtime.take_stretch(&beside)?;
        let end = at + written.length;
        if part == MidiPart::Bass
            && let Some(device) = written.device
        {
            // The synth's own bass, in place of the plain sawtooth it starts with.
            runtime.session.lock().apply_preset(device, "bass").map_err(|error| error.to_string())?;
        }
        // The notes three decibels under the stem, by the instrument's own volume: beside a mastered stem a
        // synth at the level it starts at is not heard at all.
        let notes_level = level(&made.sound(at, end, Some(vec![written.track]))?);
        let stem_level = level(&made.sound(at, end, Some(vec![stretch.track]))?);
        if let Some(device) = written.device {
            let mut session = runtime.session.lock();
            let starts_at = if part == MidiPart::Bass { -13.0 } else { 0.0 };
            session
                .edit(|project| {
                    let params = &mut project.device_mut(device)?.params;
                    let now = params.get("volume").copied().unwrap_or(starts_at);
                    params.insert("volume".to_owned(), (now + stem_level - 3.0 - notes_level).clamp(-70.0, 6.0));
                    Ok(())
                })
                .map_err(|error| error.to_string())?;
        }
        let bars = format!("bars {} to {}", first + 1, first + 8);
        let says = format!(
            "{} notes of the {word}, {bars}, on {instrument_name}, its volume set so that the notes are three \
             decibels under the stem",
            written.notes
        );
        made.render(&format!("5 notes/{word} - the notes alone.wav"), at, end, Some(vec![written.track]), &says)?;
        let says = format!("the {stem} stem, {bars}, untouched");
        made.render(&format!("5 notes/{word} - the stem alone.wav"), at, end, Some(vec![stretch.track]), &says)?;
        made.render(
            &format!("5 notes/{word} - the two together.wav"),
            at,
            end,
            None,
            "the notes and the stem together",
        )?;
    }

    let list = out.join("LIST.md");
    std::fs::write(&list, &made.list).map_err(|error| format!("{}: {error}", list.display()))?;
    println!("{}", list.display());
    Ok(())
}
