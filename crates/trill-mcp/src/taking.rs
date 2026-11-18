//! `take_from_study`: what a study does to a song. The arguments say what
//! and where; all of it is done by `trill_runtime::taking`, which the
//! window uses too.

use crate::server::{Reply, TrillServer, explain, finish};
use crate::view::round;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use trill_core::{DeviceId, TrackId};
use trill_listen::{Heard, MidiPart};
use trill_runtime::Studied;
use trill_runtime::taking::{self, HitTo, Lane, Pitch, TakeNotes, TakeStretch};

/// What `take_from_study` takes.
#[derive(Copy, Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TakeWhat {
    /// A stretch of one stem or of several (`stems`; none is the whole recording), from `from_bar` to `to_bar`
    /// of the recording or between `from_seconds` and `to_seconds`, as an audio clip that follows the song.
    Stretch,
    /// One of the study's loops, by its `id` (listen with show "loops" lists them).
    Loop,
    /// A sung line as that stretch of the vocal stem, named by its words: `line` is its number as listen with
    /// show "words" counts them, or give `near_seconds` and the line sung then is taken. It is widened to the
    /// bar lines around it, so that it keeps its place in the bar.
    Line,
    /// One word of a line (`line` and `word`, both from 1) as that stretch of the vocal stem, exactly.
    Word,
    /// A single hit or bass note, by its `id` (listen with show "one_shots"): onto a lane as audio, onto a pad
    /// of a drum rack (`drum_rack`, and `pad` for another pad than its kind's own), or into a sampler
    /// (`sampler`, or `new_sampler`) that then plays it by its own pitch.
    OneShot,
    /// The study's cleanest kick, snare, hats, cymbal, tom and percussion as one drum rack on a new track.
    Kit,
    /// A part as notes (`part`: "bass", "melody", "groove" or "chords") from `from_bar` to `to_bar`: onto
    /// `track`, where a groove asks that track's kit for its drums by name, or onto a new track.
    Notes,
    /// The parts of the recording as the song's markers.
    Parts,
    /// The recording's tempo as the song's, bar by bar, and its beats to a bar. Nothing in the song moves.
    Tempo,
    /// The key that was read in the recording, as the key the song says it is in.
    Key,
    /// The recording itself on a reference track: beside the song, silent unless listened to (update_track
    /// with `listening`), and in no render. With `with_tempo` the song takes its tempo in the same step.
    Reference,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TakeRequest {
    /// The study, by its id (the study tool lists them).
    pub study: String,
    /// What to take: "stretch", "loop", "line", "word", "one_shot", "kit", "notes", "parts", "tempo", "key" or
    /// "reference".
    pub what: TakeWhat,
    /// For a stretch: the stems it is of ("drums", "bass", "vocals", "other"). One stem is a clip of that stem's
    /// own file; several are added up into one file; none is the whole recording.
    #[serde(default)]
    pub stems: Vec<String>,
    /// The first bar of the recording to take, counted from 1 at its first bar line as listen counts them.
    pub from_bar: Option<i64>,
    /// The last bar to take (default: the same bar).
    pub to_bar: Option<i64>,
    /// Or where it begins and ends in seconds of the recording, for a stretch that is not whole bars.
    pub from_seconds: Option<f64>,
    pub to_seconds: Option<f64>,
    /// The id of a loop or of a single hit.
    pub id: Option<String>,
    /// The number of a sung line, from 1.
    pub line: Option<usize>,
    /// The number of a word in that line, from 1.
    pub word: Option<usize>,
    /// Take the line that is sung at this moment of the recording, in seconds.
    pub near_seconds: Option<f64>,
    /// For notes: "bass", "melody", "groove" or "chords".
    pub part: Option<crate::listening::TranscribePart>,
    /// For notes: move note starts onto this many steps to a beat (4 is sixteenths).
    pub quantize: Option<u32>,
    /// For notes on a new track: its instrument (a device URI; default "builtin:polysynth", the drum kit for a
    /// groove).
    pub instrument: Option<String>,
    /// The track it goes on. Left out, a track is made for it.
    pub track: Option<u64>,
    /// Where in the song it begins, as a bar counted from 1 (default 1). For parts, tempo and a reference: the
    /// bar the recording's first bar line is put on.
    pub at_bar: Option<u32>,
    /// Or as a beat of the song, counted from 0.
    pub at: Option<f64>,
    /// How many times a stretch or a loop is laid end to end (default 1).
    pub times: Option<u32>,
    /// true leaves a stretch at the recording's own speed, untouched: its bars are the song's only where the
    /// song has the recording's tempo. Default false: it follows the song's tempo, a beat for a beat.
    pub as_recorded: Option<bool>,
    /// Move the pitch by this many semitones.
    pub semitones: Option<f64>,
    /// true moves the pitch the shorter way from the recording's key to the key the song says it is in
    /// (set_project with `key`). Drums are left where they are.
    pub to_song_key: Option<bool>,
    /// For a single hit: the drum rack whose pad it goes on, by its device id.
    pub drum_rack: Option<u64>,
    /// And the pad, by the note that plays it (36 to 51), where it is not to be the one its kind usually has.
    pub pad: Option<u8>,
    /// For a single hit: the sampler it goes into, by its device id.
    pub sampler: Option<u64>,
    /// For a single hit: true makes a track with a sampler for it.
    pub new_sampler: Option<bool>,
    /// For a reference: true takes the recording's tempo into the song in the same step.
    pub with_tempo: Option<bool>,
    /// What the clip or the track is called. Left out, it is named after what it is.
    pub name: Option<String>,
}

fn said(out: &mut Value, notes: &[String]) {
    if !notes.is_empty() {
        out["said"] = json!(notes);
    }
}

impl TrillServer {
    /// The beat of the song a request names: `at`, or the start of `at_bar`.
    fn at(&self, request: &TakeRequest) -> Result<f64, String> {
        match (request.at, request.at_bar) {
            (Some(_), Some(_)) => Err("give `at` (a beat) or `at_bar` (a bar), not both".to_owned()),
            (Some(at), None) if at.is_finite() && at >= 0.0 => Ok(at),
            (Some(at), None) => Err(format!("{at} is not a beat of the song: beats are counted from 0")),
            (None, Some(0)) => Err("bars are counted from 1".to_owned()),
            (None, Some(bar)) => Ok(self.session.lock().project().meter_map().bar_start(bar - 1)),
            (None, None) => Ok(0.0),
        }
    }

    /// The stretch of the recording a request names, in seconds.
    fn stretch(&self, request: &TakeRequest, heard: &Heard) -> Result<(f64, f64), String> {
        match (request.from_bar, request.from_seconds) {
            (Some(_), Some(_)) => Err("give the stretch in bars or in seconds, not both".to_owned()),
            (Some(from), None) => {
                let to = request.to_bar.unwrap_or(from);
                if from < 1 || to < from {
                    return Err(format!(
                        "bars {from} to {to}: bars are counted from 1, and the last is not before the first"
                    ));
                }
                let (start, end) = (taking::bar_start(heard, from), taking::bar_start(heard, to + 1));
                if start >= heard.seconds - 0.01 {
                    return Err(format!(
                        "the recording has {} bars: bar {from} is past its end",
                        taking::bar_of(heard, heard.seconds - 0.01)
                    ));
                }
                Ok((start.max(0.0), end.min(heard.seconds)))
            }
            (None, Some(from)) => match request.to_seconds {
                Some(to) if to > from => Ok((from, to)),
                _ => Err("a stretch in seconds needs `to_seconds`, after `from_seconds`".to_owned()),
            },
            (None, None) => {
                Err("say which stretch of the recording: `from_bar` (and `to_bar`), or `from_seconds` and `to_seconds`"
                    .to_owned())
            }
        }
    }

    fn pitch(request: &TakeRequest) -> Result<Pitch, String> {
        match (request.semitones, request.to_song_key.unwrap_or(false)) {
            (Some(_), true) => Err("give `semitones` or `to_song_key`, not both".to_owned()),
            (Some(semitones), false) => Ok(Pitch::Semitones(semitones)),
            (None, true) => Ok(Pitch::SongKey),
            (None, false) => Ok(Pitch::AsRecorded),
        }
    }

    fn lane(request: &TakeRequest) -> Lane {
        request.track.map_or(Lane::New, |track| Lane::Track(TrackId(track)))
    }

    /// Takes a stretch that was worked out, and says what came of it.
    fn taken_stretch(&self, request: &TakeRequest, mut take: TakeStretch) -> Reply {
        take.lane = Self::lane(request);
        take.at = self.at(request)?;
        take.as_recorded = request.as_recorded.unwrap_or(false);
        take.pitch = Self::pitch(request)?;
        take.times = request.times.unwrap_or(1);
        if take.name.is_none() {
            take.name.clone_from(&request.name);
        }
        let taken = self.runtime.take_stretch(&take).map_err(|error| self.with_ids(error))?;
        let plan = &taken.plan;
        let mut out = json!({
            "track_id": taken.track,
            "clip_ids": taken.clips,
            "name": plan.name,
            "at": round(take.at),
            "beats": round(plan.length),
            "file": plan.file.display().to_string(),
            "warp": match plan.warp {
                trill_core::project::WarpMode::Off => "off",
                trill_core::project::WarpMode::Repitch => "repitch",
                trill_core::project::WarpMode::Beats => "beats",
                trill_core::project::WarpMode::Tonal => "tonal",
            },
        });
        if let Some(tempo) = plan.pieces.first().and_then(|piece| piece.source_tempo) {
            out["its_tempo"] = json!(round(tempo));
            out["song_tempo"] = json!(round(self.session.lock().project().tempo_map().tempo_at(take.at)));
        }
        if plan.semitones != 0.0 {
            out["semitones"] = json!(plan.semitones);
        }
        if take.times > 1 {
            out["times"] = json!(take.times);
        }
        said(&mut out, &plan.notes);
        Ok(out)
    }

    /// An error of the taking code with the ids there are, where it names one that is not.
    fn with_ids(&self, error: String) -> String {
        if error.ends_with("not found") {
            let session = self.session.lock();
            let project = session.project();
            let tracks: Vec<String> = project.tracks.iter().map(|track| track.id.0.to_string()).collect();
            return format!("{error}. Existing tracks: {}", tracks.join(", "));
        }
        error
    }

    async fn took(&self, request: TakeRequest) -> Reply {
        let runtime = self.runtime.clone();
        let id = request.study.trim().to_owned();
        // Reading a study is reading files: off the threads that serve the other calls.
        let read = {
            let (runtime, id) = (runtime.clone(), id.clone());
            tokio::task::spawn_blocking(move || {
                let studied = runtime.study(&id)?;
                match studied.heard.clone() {
                    Some(heard) if studied.report.study.state == trill_study::State::Done => Ok((heard, studied)),
                    _ => Err(crate::studying::not_done(&studied.report)),
                }
            })
        };
        let (heard, studied): (Arc<Heard>, Studied) =
            read.await.map_err(|error| format!("reading the study stopped: {error}"))??;
        let heard = &*heard;
        match request.what {
            TakeWhat::Stretch => {
                let (from, to) = self.stretch(&request, heard)?;
                self.taken_stretch(&request, TakeStretch::new(id, request.stems.clone(), from, to))
            }
            TakeWhat::Loop => {
                let wanted = request
                    .id
                    .as_deref()
                    .ok_or("say which loop: its `id`, as listen with show \"loops\" lists them")?;
                let Some(found) = heard.loops.iter().find(|found| found.id == wanted) else {
                    let some: Vec<&str> = heard.loops.iter().take(8).map(|found| found.id.as_str()).collect();
                    return Err(format!(
                        "the study {id} has no loop '{wanted}'; it has {}, such as {}",
                        heard.loops.len(),
                        some.join(", ")
                    ));
                };
                let mut take = TakeStretch::new(id, found.stems.clone(), found.start, found.end);
                take.name = request.name.clone().or_else(|| {
                    Some(format!(
                        "{} loop, {} bars from bar {} ({})",
                        found.label,
                        found.bars,
                        found.bar + 1,
                        studied.report.study.title
                    ))
                });
                self.taken_stretch(&request, take)
            }
            TakeWhat::Line | TakeWhat::Word => {
                let Some(lyrics) = studied.lyrics.as_ref().filter(|lyrics| !lyrics.lines.is_empty()) else {
                    return Err(format!("the study {id} wrote down no words: none were asked for, or nobody sings"));
                };
                let number = match (request.line, request.near_seconds) {
                    (Some(line), _) => line,
                    (None, Some(near)) => {
                        let distance = |line: &trill_study::lyrics::Line| {
                            if near < line.start { line.start - near } else { (near - line.end).max(0.0) }
                        };
                        let nearest =
                            lyrics.lines.iter().enumerate().min_by(|a, b| distance(a.1).total_cmp(&distance(b.1)));
                        nearest.map_or(1, |(index, _)| index + 1)
                    }
                    (None, None) => {
                        return Err(
                            "say which line: `line` (its number, from 1) or `near_seconds` (a moment it is sung at)"
                                .to_owned(),
                        );
                    }
                };
                let Some(line) = number.checked_sub(1).and_then(|index| lyrics.lines.get(index)) else {
                    return Err(format!("the study {id} has {} lines; lines are counted from 1", lyrics.lines.len()));
                };
                let (from, to, words) = if request.what == TakeWhat::Word {
                    let Some(word) =
                        request.word.and_then(|word| word.checked_sub(1)).and_then(|index| line.words.get(index))
                    else {
                        return Err(format!(
                            "line {number} has {} words (\"{}\"); say which with `word`, counted from 1",
                            line.words.len(),
                            line.text
                        ));
                    };
                    (word.start, word.end, word.text.trim().to_owned())
                } else {
                    let (from, to) = taking::whole_bars(heard, line.start, line.end);
                    (from, to, line.text.trim().to_owned())
                };
                let mut take = TakeStretch::new(id, vec!["vocals".to_owned()], from, to);
                take.name = request.name.clone().or(Some(words.clone()));
                let mut out = self.taken_stretch(&request, take)?;
                out["words"] = json!(words);
                out["line"] = json!(number);
                Ok(out)
            }
            TakeWhat::OneShot => {
                let wanted = request
                    .id
                    .as_deref()
                    .ok_or("say which hit: its `id`, as listen with show \"one_shots\" lists them")?;
                let at = self.at(&request)?;
                let to = match (request.drum_rack, request.sampler, request.new_sampler.unwrap_or(false)) {
                    (Some(rack), None, false) => HitTo::Pad(DeviceId(rack), request.pad),
                    (None, Some(sampler), false) => HitTo::Sampler(DeviceId(sampler)),
                    (None, None, true) => HitTo::NewSampler,
                    (None, None, false) => HitTo::Lane(Self::lane(&request), at),
                    _ => {
                        return Err(
                            "give one of `drum_rack`, `sampler` and `new_sampler`, or none for a lane".to_owned()
                        );
                    }
                };
                let runtime = runtime.clone();
                let wanted = wanted.to_owned();
                let taken = tokio::task::spawn_blocking(move || runtime.take_hit(&id, &wanted, to))
                    .await
                    .map_err(|error| format!("taking the hit stopped: {error}"))?
                    .map_err(|error| self.with_ids(error))?;
                let mut out = json!({ "label": taken.label, "file": taken.file.display().to_string() });
                for (key, value) in [
                    ("track_id", taken.track.map(|track| json!(track))),
                    ("clip_id", taken.clip.map(|clip| json!(clip))),
                    ("device_id", taken.device.map(|device| json!(device))),
                ] {
                    if let Some(value) = value {
                        out[key] = value;
                    }
                }
                match (to, taken.note) {
                    (HitTo::Pad(..), Some(note)) => out["pad"] = json!(note),
                    (HitTo::Sampler(_) | HitTo::NewSampler, Some(note)) => {
                        out["plays_unchanged_on"] = json!(trill_core::pitch::pitch_name(note));
                    }
                    _ => {}
                }
                Ok(out)
            }
            TakeWhat::Kit => {
                let name = request.name.clone();
                let taken = tokio::task::spawn_blocking(move || runtime.take_kit(&id, name.as_deref()))
                    .await
                    .map_err(|error| format!("taking the kit stopped: {error}"))??;
                let pads: Vec<Value> = taken
                    .pads
                    .iter()
                    .map(|(note, label, _)| json!({ "pad": note, "drum": trill_core::pitch::drum_name(*note), "hit": label }))
                    .collect();
                Ok(json!({ "track_id": taken.track, "device_id": taken.device, "pads": pads }))
            }
            TakeWhat::Notes => {
                let part = match request.part {
                    Some(crate::listening::TranscribePart::Bass) => MidiPart::Bass,
                    Some(crate::listening::TranscribePart::Melody) => MidiPart::Melody,
                    Some(crate::listening::TranscribePart::Groove) => MidiPart::Groove,
                    Some(crate::listening::TranscribePart::Chords) => MidiPart::Chords,
                    None => return Err("say which `part`: \"bass\", \"melody\", \"groove\" or \"chords\"".to_owned()),
                };
                let (from, to) = match (request.from_bar, request.from_seconds) {
                    (None, None) => (taking::bar_start(heard, 1).max(0.0), heard.seconds),
                    _ => self.stretch(&request, heard)?,
                };
                let take = TakeNotes {
                    study: id,
                    part,
                    from,
                    to,
                    lane: Self::lane(&request),
                    at: self.at(&request)?,
                    quantize: request.quantize,
                    instrument: request.instrument.clone(),
                };
                let keyed = |session: &trill_engine::Session, track: TrackId, notes: &[trill_core::Note]| {
                    let how = vec![None; notes.len()];
                    Ok(crate::instruments::played(session, track, &[], notes, &how)?.map(|played| played.all))
                };
                let taken = self.runtime.take_notes(&take, Some(&keyed)).map_err(|error| self.with_ids(error))?;
                let mut out = json!({
                    "part": part.name(),
                    "track_id": taken.track,
                    "clip_id": taken.clip,
                    "notes": taken.notes,
                    "beats": round(taken.length),
                });
                if let Some(device) = taken.device {
                    out["instrument_device_id"] = json!(device);
                }
                if !taken.lacking.is_empty() {
                    out["left_out"] = json!(format!(
                        "this track's kit has no {}: those hits were left out",
                        taken.lacking.join(", ")
                    ));
                }
                said(&mut out, &taken.said);
                Ok(out)
            }
            TakeWhat::Parts => {
                let at = self.at(&request)?;
                let markers = self.runtime.take_parts(&id, at)?;
                let session = self.session.lock();
                let bars = session.project().meter_map();
                let listed: Vec<Value> = markers
                    .iter()
                    .map(|(beat, name)| json!({ "bar": bars.bar_at(*beat).number(), "name": name }))
                    .collect();
                Ok(json!({ "markers": listed }))
            }
            TakeWhat::Tempo => {
                let at = self.at(&request)?;
                let taken = self.runtime.take_tempo(&id, at)?;
                Ok(tempo_json(&taken))
            }
            TakeWhat::Key => {
                let Some(key) = taking::key_of(heard) else {
                    return Err(format!("no key was read in the study {id}: there was too little pitched sound"));
                };
                let mut session = self.session.lock();
                session
                    .edit(|project| {
                        project.set_key(Some(key));
                        Ok(())
                    })
                    .map_err(|error| explain(session.project(), error))?;
                Ok(json!({ "key": key.name() }))
            }
            TakeWhat::Reference => {
                let at = self.at(&request)?;
                let with_tempo = request.with_tempo.unwrap_or(false);
                let taken = tokio::task::spawn_blocking(move || runtime.take_reference(&id, at, with_tempo))
                    .await
                    .map_err(|error| format!("taking the reference stopped: {error}"))??;
                let mut out = json!({
                    "track_id": taken.track,
                    "clip_id": taken.clip,
                    "file": taken.file.display().to_string(),
                    "listening": false,
                    "note": "a reference is silent until it is listened to (update_track with `listening`), is heard \
                             in place of the song while it is, and is in no render; measure with `only_tracks` \
                             naming it measures the recording itself",
                });
                if let Some(tempo) = &taken.tempo {
                    out["tempo"] = tempo_json(tempo);
                }
                said(&mut out, &taken.said);
                Ok(out)
            }
        }
    }
}

fn tempo_json(taken: &taking::TakenTempo) -> Value {
    let mut out = json!({
        "bars": taken.bars,
        "tempos": taken.tempos,
        "from_beat": round(taken.from),
        "to_beat": round(taken.to),
        "beats_per_bar": taken.beats_per_bar,
    });
    if taken.tempos == 1 {
        out["bpm"] = json!(round(taken.slowest));
    } else {
        out["slowest_bpm"] = json!(round(taken.slowest));
        out["fastest_bpm"] = json!(round(taken.fastest));
    }
    out
}

#[tool_router(router = taking_router, vis = "pub(crate)")]
impl TrillServer {
    /// Take something a study found into the song, as one undo step: a stretch of its stems or one of its loops
    /// as an audio clip that follows the song's tempo (and its key, where asked), a sung line or a word as
    /// that stretch of the vocal, a single hit onto a lane, a drum rack's pad or a sampler, a whole kit as a
    /// drum rack, a part as notes on an instrument track, its parts as markers, its tempo bar by bar, its key,
    /// or the recording itself as a reference track that is never rendered. Read the study with `listen` first:
    /// bars, ids and line numbers are the ones it lists.
    #[tool(annotations(title = "Take from a study"))]
    async fn take_from_study(&self, Parameters(request): Parameters<TakeRequest>) -> CallToolResult {
        finish(self.took(request).await)
    }
}
