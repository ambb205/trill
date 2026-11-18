//! A study's run: its stages in order, each writing what it found into
//! the study's folder.
//!
//! A stage whose product is in the folder is done and is not done again,
//! so a run that was stopped, or that failed, is taken up where it was.
//! Everything is written under another name until it is whole: a run that
//! is ended at any moment leaves nothing half-written under a name that
//! counts.
//!
//! This runs in a process of its own ([`crate::helper`]); nothing here
//! knows that.

use crate::lyrics::{self, Envelope, Lyrics};
use crate::models::Models;
use crate::store::{LONGEST_SECONDS, Source, Stage, StageState, State, StudyFolder, partial_of};
use std::time::Instant;
use trill_core::audio::AudioBuffer;
use trill_listen::{Options, RATE, Recording, Sound, Stem};
use trill_media::WavFormat;

/// A recording in a format Trill does not read, as `ffmpeg` wrote it out:
/// kept only until the stems are written.
const CONVERTED: &str = "converted.wav";

/// What a run says as it goes.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A stage has begun.
    Stage(Stage),
    /// How far the stage has come, 0 to 1.
    Progress(f32),
    /// What runs the models.
    Device(String),
}

/// The programs a run may use that are not Trill's own: found by whoever
/// starts it, never fetched.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Programs {
    /// `ffmpeg`, for a recording in a format Trill does not read itself.
    #[serde(default)]
    pub ffmpeg: Option<std::path::PathBuf>,
    /// The program that fetches a recording from a link.
    #[serde(default)]
    pub fetcher: Option<std::path::PathBuf>,
}

/// One stage's turn: marks it running, does it, and marks how it went.
/// `work` answers with a note on how it went, or `None` when the stage
/// was not wanted.
fn stage(
    folder: &StudyFolder,
    stage: Stage,
    said: &mut dyn FnMut(Event),
    work: impl FnOnce(&mut dyn FnMut(Event)) -> Result<Option<String>, String>,
) -> Result<(), String> {
    let mut study = folder.study()?;
    let report = study.report(stage);
    (report.state, report.seconds, report.note) = (StageState::Running, None, String::new());
    folder.write(&study)?;
    said(Event::Stage(stage));
    let begun = Instant::now();
    let went = work(said);
    // The work may have written to the study itself: read it again.
    let mut study = folder.study()?;
    let seconds = (begun.elapsed().as_secs_f64() * 10.0).round() / 10.0;
    let outcome = match went {
        Ok(note) => {
            let report = study.report(stage);
            report.seconds = Some(seconds);
            match note {
                Some(note) => (report.state, report.note) = (StageState::Done, note),
                None => report.state = StageState::Skipped,
            }
            Ok(())
        }
        Err(why) => {
            let report = study.report(stage);
            (report.state, report.seconds) = (StageState::Failed, Some(seconds));
            let sentence = format!("{} failed: {why}", stage.doing());
            (study.state, study.error) = (State::Failed, Some(sentence.clone()));
            Err(sentence)
        }
    };
    folder.write(&study)?;
    outcome
}

/// Has the models ready before a stage asks anything of them, and says
/// and keeps what runs them.
fn using(folder: &StudyFolder, models: &mut dyn Models, said: &mut dyn FnMut(Event)) -> Result<(), String> {
    models.ready()?;
    let device = models.device();
    let mut study = folder.study()?;
    if study.device.as_deref() != Some(device.as_str()) {
        study.device = Some(device.clone());
        folder.write(&study)?;
    }
    said(Event::Device(device));
    Ok(())
}

/// Reads the recording a study is of, at the rate everything works at.
fn read(folder: &StudyFolder, programs: &Programs) -> Result<Sound, String> {
    let study = folder.study()?;
    let path = match (folder.kept_source(), &study.source) {
        (Some(kept), _) => kept,
        (None, Source::File { path }) => path.clone(),
        (None, Source::Link { url, .. }) => return Err(format!("the recording behind {url} has not been fetched")),
    };
    let buffer = match trill_media::decode(&path) {
        Ok(buffer) => buffer,
        Err(error) => match &programs.ffmpeg {
            // A format Trill does not read itself: `ffmpeg` does, where it is installed.
            Some(ffmpeg) if path.is_file() => {
                let converted = crate::link::convert(ffmpeg, &path, &folder.path.join(CONVERTED))?;
                trill_media::decode(&converted).map_err(|error| error.to_string())?
            }
            _ => {
                return Err(format!(
                    "{error}. Trill reads WAV, AIFF, FLAC, MP3 and Ogg Vorbis itself, and anything else through ffmpeg where it is installed"
                ));
            }
        },
    };
    let seconds = buffer.seconds();
    if seconds < 2.0 {
        return Err(format!("there are {seconds:.1} seconds of audio in {}: too little to study", path.display()));
    }
    if seconds > LONGEST_SECONDS + 1.0 {
        return Err(format!(
            "{} is {:.0} minutes long, and a recording is studied up to {:.0} minutes: cut the part you want into a file of its own",
            path.display(),
            seconds / 60.0,
            LONGEST_SECONDS / 60.0
        ));
    }
    Ok(Sound::of(&buffer))
}

fn write_stem(folder: &StudyFolder, stem: &mut Stem) -> Result<(), String> {
    let path = folder.stem(&stem.name);
    let said = |error: String| format!("{}: {error}", path.display());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| said(error.to_string()))?;
    }
    let partial = partial_of(&path);
    // Lent to the writer and taken back: a stem of a long recording is not held twice to be written.
    let sound = std::mem::take(&mut stem.sound);
    let buffer = AudioBuffer { sample_rate: RATE, left: sound.left, right: sound.right };
    let written = trill_media::write_wav(&partial, &buffer, WavFormat::Int24);
    stem.sound = Sound { left: buffer.left, right: buffer.right };
    written.map_err(|error| said(error.to_string()))?;
    std::fs::rename(&partial, &path).map_err(|error| said(error.to_string()))
}

/// The stems of a study as they are on disk, or `None` when one is missing.
pub fn stems_of(folder: &StudyFolder, names: &[String]) -> Option<Vec<Stem>> {
    if names.is_empty() {
        return None;
    }
    names
        .iter()
        .map(|name| {
            let buffer = trill_media::decode(&folder.stem(name)).ok()?;
            Some(Stem { name: name.clone(), sound: Sound::of(&buffer) })
        })
        .collect()
}

/// Runs every stage of the study in `folder` that is not done yet.
pub fn run(
    folder: &StudyFolder,
    models: &mut dyn Models,
    programs: &Programs,
    said: &mut dyn FnMut(Event),
) -> Result<(), String> {
    let mut study = folder.study()?;
    (study.state, study.error) = (State::Running, None);
    // What was running when a run was ended is to be done again.
    for report in &mut study.stages {
        if matches!(report.state, StageState::Running | StageState::Failed) {
            report.state = StageState::Pending;
        }
    }
    folder.write(&study)?;
    let options = study.options.clone();

    // Stems that are there add up to the recording: it need not be read again.
    let mut stems: Option<Vec<Stem>> = stems_of(folder, &folder.study()?.stems);
    let mut mix: Option<Sound> = None;
    if let Source::Link { url, .. } = &study.source {
        stage(folder, Stage::Fetch, said, |said| {
            if stems.is_some() || folder.kept_source().is_some() {
                return Ok(Some("already done".to_owned()));
            }
            let Some(fetcher) = &programs.fetcher else {
                return Err("a link is fetched by yt-dlp, which was not found on this machine: install it, or study a file you have".to_owned());
            };
            let fetched = crate::link::fetch(
                fetcher,
                programs.ffmpeg.as_deref(),
                url,
                &folder.path,
                LONGEST_SECONDS,
                &mut |share| said(Event::Progress(share)),
            )?;
            let mut study = folder.study()?;
            // A study that was given no name of its own takes the recording's.
            if let Some(title) = fetched.title.filter(|_| study.title == *url) {
                study.title = title;
            }
            study.source = Source::Link { url: url.clone(), by: fetched.by };
            folder.write(&study)?;
            Ok(Some(String::new()))
        })?;
    }
    stage(folder, Stage::Read, said, |_| {
        let (sound, note) = match &stems {
            Some(stems) => (Recording::mix_of(stems), "already done"),
            None => (read(folder, programs)?, ""),
        };
        let mut study = folder.study()?;
        study.seconds = (sound.seconds() * 1e4).round() / 1e4;
        folder.write(&study)?;
        mix = Some(sound);
        Ok(Some(note.to_owned()))
    })?;
    let mix = mix.expect("the stage before did not fail");

    stage(folder, Stage::Stems, said, |said| {
        if stems.is_some() {
            return Ok(Some("already done".to_owned()));
        }
        using(folder, models, said)?;
        let mut found = models.stems(&mix, options.stems, &mut |share| said(Event::Progress(share * 0.9)))?;
        if found.is_empty() {
            return Err("the model gave no stems".to_owned());
        }
        // What the model leaves unassigned goes into `other`, so that the
        // stems add up to the recording: mute nothing and it is the mix.
        let frames = mix.frames();
        for stem in &mut found {
            stem.sound.left.resize(frames, 0.0);
            stem.sound.right.resize(frames, 0.0);
        }
        if let Some(other) = found.iter().position(|stem| stem.name == "other") {
            for index in 0..frames {
                let (mut left, mut right) = (mix.left[index], mix.right[index]);
                for stem in &found {
                    left -= stem.sound.left[index];
                    right -= stem.sound.right[index];
                }
                found[other].sound.left[index] += left;
                found[other].sound.right[index] += right;
            }
        }
        let count = found.len();
        for (index, stem) in found.iter_mut().enumerate() {
            write_stem(folder, stem)?;
            said(Event::Progress(0.9 + 0.1 * (index + 1) as f32 / count as f32));
        }
        folder.write_outlines(&found)?;
        let mut study = folder.study()?;
        study.stems = found.iter().map(|stem| stem.name.clone()).collect();
        folder.write(&study)?;
        stems = Some(found);
        // What ffmpeg made of a recording Trill does not read is not needed again.
        let _ = std::fs::remove_file(folder.path.join(CONVERTED));
        Ok(Some(String::new()))
    })?;
    let mut stems = stems.expect("the stage before did not fail");

    stage(folder, Stage::Beats, said, |said| {
        if folder.beats().is_some() {
            return Ok(Some("already done".to_owned()));
        }
        using(folder, models, said)?;
        let beats = models.beats(&mix.mono(), &mut |share| said(Event::Progress(share)))?;
        folder.write_beats(&beats)?;
        Ok(Some(format!("{} beats", beats.beats.len())))
    })?;

    // The words are heard in the isolated vocal: a transcriber hears them
    // far better there than in a mix, and the stem is silent wherever
    // nobody sings, which keeps it from inventing lines over the band.
    let vocal: Option<Vec<f32>> = stems.iter().find(|stem| stem.name == "vocals").map(|stem| stem.sound.mono());
    let sings = vocal.as_deref().is_some_and(|vocal| {
        options.words
            && vocal.len() as f64 >= RATE
            && vocal.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs())) >= 0.003
    });
    let singing = vocal.as_deref().filter(|_| sings);
    stage(folder, Stage::Words, said, |said| {
        let Some(vocal) = singing else { return Ok(None) };
        if folder.transcript().is_some() {
            return Ok(Some("already done".to_owned()));
        }
        using(folder, models, said)?;
        let transcript = models.words(vocal, options.language.as_deref(), &mut |share| said(Event::Progress(share)))?;
        folder.write_transcript(&transcript)?;
        Ok(Some(format!("{} words", transcript.stretches.iter().map(|stretch| stretch.words.len()).sum::<usize>())))
    })?;

    stage(folder, Stage::WordTimes, said, |said| {
        let (Some(vocal), Some(transcript)) = (singing, folder.transcript()) else {
            folder.write_lyrics(&Lyrics::default())?;
            return Ok(None);
        };
        if folder.lyrics().is_some() {
            return Ok(Some("already done".to_owned()));
        }
        let envelope = Envelope::new(vocal);
        let mut words = transcript.rough_words();
        let mut unplaced = "";
        let placed = if words.is_empty() {
            false
        } else {
            using(folder, models, said)?;
            if models.places_words() {
                unplaced = "the words are in a script the aligner does not read";
                models.word_times(vocal, &mut words, &mut |share| said(Event::Progress(share)))?
            } else {
                unplaced = "the part that places words has not been fetched";
                false
            }
        };
        lyrics::settle(&mut words);
        // A word with nothing sung under it was made up.
        let heard = words.len();
        words.retain(|word| envelope.sung(word.start, word.end));
        let invented = heard - words.len();
        envelope.refine(&mut words, placed);
        let lyrics = Lyrics { language: transcript.language.clone(), lines: lyrics::lines(words, &envelope), placed };
        folder.write_lyrics(&lyrics)?;
        let mut note = format!("{} lines", lyrics.lines.len());
        if invented > 0 {
            note.push_str(&format!("; {invented} words with nothing sung under them were left out"));
        }
        if !placed && !unplaced.is_empty() {
            note.push_str(&format!("; {unplaced}, and they are placed only as well as the transcriber heard them"));
        }
        Ok(Some(note))
    })?;

    // What is listened to is the stems: the mix and the vocal by itself were for the models.
    drop(vocal);
    drop(mix);
    stage(folder, Stage::Listen, said, |said| {
        if folder.heard().is_some() {
            return Ok(Some("already done".to_owned()));
        }
        said(Event::Progress(0.05));
        // A study made before outlines were kept gets its own now.
        if folder.outlines().is_none() {
            folder.write_outlines(&stems)?;
        }
        let options = Options { beats: folder.beats(), corrections: folder.study()?.corrections };
        let heard = trill_listen::hear(&Recording::of_stems(std::mem::take(&mut stems)), &options);
        if let Some(mut lyrics) = folder.lyrics().filter(|lyrics| !lyrics.lines.is_empty()) {
            lyrics::pitch_words(&mut lyrics, &heard.melody);
            folder.write_lyrics(&lyrics)?;
        }
        folder.write_heard(&heard)?;
        Ok(Some(String::new()))
    })?;

    let mut study = folder.study()?;
    (study.state, study.error) = (State::Done, None);
    folder.write(&study)
}
