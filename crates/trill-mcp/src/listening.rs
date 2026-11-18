//! Tools for what is in a sound.
//!
//! A model cannot hear, but it can read. `listen` turns an audio file (one
//! of the sound library, one the user names, the file of a clip) into what
//! a musician would say of it: its tempo and where its bars are, its key
//! and chords, its parts, the loops and single hits in it, its drum groove
//! and the notes of its bass and melody. `transcribe` writes one of those
//! as MIDI on the recording's own beats, into a file or straight into the
//! song.
//!
//! None of it needs a model, and all of it is surer of a sound that is one
//! instrument than of a whole mix: what a reply says of a mix it says with
//! that caveat.

use crate::server::{Reply, TrillServer, explain, finish, import_parts};
use crate::studying::{not_done, study_view};
use crate::view::round;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use trill_core::ClipId;
use trill_listen::harmony::note_name;
use trill_listen::{Correction, Heard, HitKind, MidiPart};
use trill_runtime::Studied;

/// Appended to the server's instructions.
pub(crate) const INSTRUCTIONS: &str = "\
Listening to a recording
- listen says what is in an audio file: its tempo with where its bars lie, its key with the runners-up, how far \
it sits from A = 440, and how many chords, parts, loops, single hits and notes were found. Give it a `path` (a \
sound from search_sounds, a file the user names) or an audio `clip` of the song. Nothing is changed and no model \
is used. A song's worth of audio takes some seconds the first time; asking again of the same file is immediate.
- Ask for what you want listed with `show`: \"chords\" (a chord for each stretch of beats), \"parts\" (where the \
song changes, lettered so that parts that return share a letter: A B A B C B), \"loops\" (whole bars that come \
back round, best first, with how many times each pattern returns), \"one_shots\" (the cleanest drum hits of each \
kind and a bass note for each pitch), \"groove\" (the drum part bar by bar, in beats of the bar), \"bass\" and \
\"melody\" (their notes), \"bars\" (every bar line with its own tempo). Positions are in the recording's own bars, \
counted from 1 at its first bar line; `from_bar` and `to_bar` keep a list to a stretch, and lists are paged \
(`more` is the next `offset`).
- This is how to use a reference: read its parts, then the chords and the groove of the part you care about, and \
write your own with them in mind. What you read is of someone's recording: take its tempo, its form and how its \
groove sits, not its melody.
- The tracker can have the beat wrong in three ways, and `grid` rereads it: \"half\" or \"double\" the tempo, \
\"rotate\" to start the bar one beat later, \"bar3\" (to \"bar12\") for that many beats in a bar. Everything else \
follows the corrected grid. Without a model the bar line is the least sure thing in a reply: if the chords seem \
to change in the middle of bars, rotate.
- All of it is surer of one instrument than of a whole mix. In a mix the bass plays in the kick's band and other \
instruments in the snare's, so the groove of a mix is a sketch, and bass and melody are what was loudest in \
their registers. Of a drum loop, a bass line or a voice alone they are right.
- transcribe writes one of \"bass\", \"melody\", \"groove\" or \"chords\" as MIDI, placed by the recording's own \
beats so that it stays in time where the recording's tempo wanders: into a MIDI `file`, or into the song from \
`at` beats on, on a new track with an instrument (one undo step). The groove is written on sixteenths as General \
MIDI kick, snare and closed hat.";

/// How many items a list holds when the caller does not say, and the most
/// it can ask for.
const DEFAULT_ITEMS: usize = 40;
const MAX_ITEMS: usize = 200;

/// What `listen` lists beside its summary.
#[derive(Copy, Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ListenShow {
    /// A chord for each stretch of beats: where it starts (bar and beat), how many beats it lasts, its name.
    Chords,
    /// The parts of the recording: letter, first bar, bars, and the stems that rest in it when it was taken apart.
    Parts,
    /// The stretches worth looping, best first: bars, where, their own tempo, how well they close, how often they return.
    Loops,
    /// Single hits and notes worth keeping: the cleanest of each kind of drum, one bass note for each pitch.
    OneShots,
    /// The drum part, bar by bar: the beats of the bar (1 is its first; 2.5 the off-beat after 2) on which the
    /// kick, the snare and the hat are struck.
    Groove,
    /// The notes of the bass line: bar, beat, length in beats, note.
    Bass,
    /// The notes of the sung or played melody, likewise.
    Melody,
    /// Every bar line: its time in seconds and the tempo of its bar.
    Bars,
    /// Of a study: the words that are sung, line by line, each line with where it starts and how long it is, and
    /// every word of it as [bar, beat, word].
    Words,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListenRequest {
    /// The whole path of an audio file (WAV, AIFF, FLAC, MP3, Ogg Vorbis), up to 15 minutes long: a `path` from
    /// search_sounds, or a file the user names.
    pub path: Option<String>,
    /// Or an audio clip of the song, by its id: the file it plays is listened to, from its start.
    pub clip: Option<u64>,
    /// Or a study, by its id (the study tool makes one): what was found in the recording's stems, which is right
    /// of a whole mix where listening to the file is a sketch, and has the words.
    pub study: Option<String>,
    /// Of a study: only the loops and single hits of this stem ("vocals", "drums", "bass", "other"), with the
    /// stem's file and the bars it plays in.
    pub stem: Option<String>,
    /// What to list beside the summary: any of "chords", "parts", "loops", "one_shots", "groove", "bass",
    /// "melody", "bars", and of a study "words". Leave it out for the summary alone, which says how many of each
    /// there are.
    #[serde(default)]
    pub show: Vec<ListenShow>,
    /// Only what lies from this bar of the recording on (1 is its first bar line).
    pub from_bar: Option<i64>,
    /// Only what lies up to the end of this bar.
    pub to_bar: Option<i64>,
    /// Skip this many items of each list: a reply's `more` says where the next page starts.
    pub offset: Option<usize>,
    /// How many items each list holds (default 40, at most 200).
    pub limit: Option<usize>,
    /// Corrections to the beat grid, made in this order: "half", "double", "rotate" (the bar starts one beat
    /// later), "bar3" to "bar12" (beats in a bar). Given for a study they are kept with it, and everything is
    /// heard again over the corrected grid (some seconds); "none" takes them off again.
    #[serde(default)]
    pub grid: Vec<String>,
}

/// What `transcribe` writes.
#[derive(Copy, Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscribePart {
    /// The bass line, as played.
    Bass,
    /// The sung or played melody, as played.
    Melody,
    /// The drum part on sixteenths: General MIDI kick (36), snare (38) and closed hat (42).
    Groove,
    /// Each chord in close position from C3, with its root an octave below.
    Chords,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TranscribeRequest {
    /// The whole path of the audio file to transcribe from.
    pub path: Option<String>,
    /// Or an audio clip of the song, by its id.
    pub clip: Option<u64>,
    /// Or a study, by its id: what was heard in its stems.
    pub study: Option<String>,
    /// What to write: "bass", "melody", "groove" or "chords".
    pub part: TranscribePart,
    /// The first bar of the recording to write (1 is its first bar line, and the default): it becomes beat 0.
    pub from_bar: Option<i64>,
    /// The last bar to write (default: to the end).
    pub to_bar: Option<i64>,
    /// Move note starts onto this many steps to a beat (4 is sixteenths). The groove is on sixteenths unless you
    /// say otherwise; bass, melody and chords are left as played unless you say.
    pub quantize: Option<u32>,
    /// Corrections to the beat grid, as listen takes them.
    #[serde(default)]
    pub grid: Vec<String>,
    /// Write a MIDI file here (.mid).
    pub file: Option<String>,
    /// Or put it into the song from this beat on, on a new track with an instrument and one clip.
    pub at: Option<f64>,
    /// The instrument of that new track (a device URI; default "builtin:polysynth"). A groove gets the drum kit.
    pub instrument: Option<String>,
}

/// Bars and beats of a recording, counted from 1 at its first bar line.
struct Counted<'a> {
    heard: &'a Heard,
    /// The beat of the grid that starts bar 1.
    first: f64,
    per_bar: f64,
}

impl<'a> Counted<'a> {
    fn new(heard: &'a Heard) -> Self {
        Self {
            first: heard.grid.bars.first().copied().unwrap_or(0) as f64,
            per_bar: heard.grid.beats_per_bar.max(1) as f64,
            heard,
        }
    }

    /// The bar a time lies in and its beat in that bar, both from 1.
    fn place(&self, seconds: f64) -> (i64, f64) {
        // A hair past the line counts as on it.
        let beats = self.heard.grid.beat_at(seconds) - self.first + 1e-6;
        let bar = (beats / self.per_bar).floor();
        (bar as i64 + 1, ((beats - bar * self.per_bar) * 100.0).round() / 100.0 + 1.0)
    }

    fn beats_between(&self, start: f64, end: f64) -> f64 {
        ((self.heard.grid.beat_at(end) - self.heard.grid.beat_at(start)) * 100.0).round() / 100.0
    }

    /// The seconds at which bar `bar` (from 1) starts.
    fn start_of(&self, bar: i64) -> f64 {
        self.heard.grid.seconds_at(self.first + (bar - 1) as f64 * self.per_bar)
    }

    /// The stretch of the recording the caller asked for, in seconds.
    fn stretch(&self, from_bar: Option<i64>, to_bar: Option<i64>) -> (f64, f64) {
        let start = from_bar.map_or(0.0, |bar| self.start_of(bar).max(0.0));
        let end = to_bar.map_or(self.heard.seconds, |bar| self.start_of(bar + 1).min(self.heard.seconds));
        (start, end.max(start))
    }
}

/// One page of `items`, and where the next begins.
fn page(items: Vec<Value>, offset: usize, limit: usize) -> Value {
    let total = items.len();
    let shown: Vec<Value> = items.into_iter().skip(offset).take(limit).collect();
    let mut out = json!({ "total": total, "items": shown });
    if offset + limit < total {
        out["more"] = json!(offset + limit);
    }
    out
}

fn corrections(texts: &[String]) -> Result<Vec<Correction>, String> {
    texts
        .iter()
        .map(|text| {
            Correction::parse(text).ok_or_else(|| {
                format!("'{text}' is not a correction to a beat grid; use \"half\", \"double\", \"rotate\", or \"bar3\" to \"bar12\"")
            })
        })
        .collect()
}

/// What a request listens to.
enum Sound {
    /// An audio file, by its path.
    File(String),
    /// A study, by its id.
    Study(String),
}

/// What the whole of a reply says first.
fn summary(heard: &Heard) -> Value {
    let grid = &heard.grid;
    let mut out = json!({ "seconds": round(heard.seconds) });
    if grid.beats.is_empty() {
        out["tempo"] = Value::Null;
        out["note"] = json!(
            "No pulse was found: a single hit, or a sound without a beat, has no tempo and no bars. What is listed \
             is placed in seconds."
        );
    } else {
        out["tempo"] = json!({
            "bpm": grid.bpm,
            "beats_per_bar": grid.beats_per_bar,
            "bars": grid.bars.len(),
            "first_bar_at_seconds": round(grid.bars.first().and_then(|&index| grid.beats.get(index)).copied().unwrap_or(0.0)),
            "steady": grid.steady,
            "wanders_percent": round(grid.drift * 100.0),
        });
    }
    if let Some(reading) = heard.key() {
        out["key"] = json!({
            "name": reading.key.name(),
            "confidence": reading.confidence,
            "also": reading.alternates.iter().map(|other| other.key.name()).collect::<Vec<_>>(),
            "camelot": reading.key.camelot(),
        });
        out["tuning_cents"] = json!(heard.harmony.as_ref().map_or(0.0, |harmony| harmony.tuning_cents));
    }
    if !heard.parts.is_empty() {
        out["form"] =
            json!(heard.parts.iter().map(|part| format!("{}{}", part.label, part.bars)).collect::<Vec<_>>().join(" "));
    }
    out["found"] = json!({
        "chords": heard.harmony.as_ref().map_or(0, |harmony| harmony.chords.len()),
        "parts": heard.parts.len(),
        "loops": heard.loops.len(),
        "one_shots": heard.one_shots.len(),
        "groove": heard.groove.len(),
        "bass": heard.bass.len(),
        "melody": heard.melody.len(),
    });
    out
}

/// What a reply says of a study before what was heard in it, and of the
/// stem that was asked for.
fn of_study(out: &mut Value, heard: &Heard, studied: &Studied, stem: Option<&str>) -> Result<(), String> {
    let view = study_view(&studied.report, &studied.folder);
    for key in ["study", "title", "of", "by", "stems", "grid", "ran_on"] {
        if let Some(value) = view.get(key) {
            out[key] = value.clone();
        }
    }
    let lines = studied.lyrics.as_ref().map_or(0, |lyrics| lyrics.lines.len());
    out["found"]["words"] = json!(lines);
    if let Some(lyrics) = studied.lyrics.as_ref().filter(|lyrics| !lyrics.lines.is_empty()) {
        out["words"] = json!({
            "lines": lines,
            "language": lyrics.language,
            "each_word_placed": lyrics.placed,
        });
    }
    out["heard_as"] = json!(format!(
        "the stems a model split the recording into ({}): the groove is the drums', the bass line the bass's, the melody and the words the voice's",
        heard.stems.join(", ")
    ));
    let Some(stem) = stem else { return Ok(()) };
    if !heard.stems.iter().any(|name| name == stem) {
        return Err(format!("the study has no stem '{stem}'. Stems: {}", heard.stems.join(", ")));
    }
    // The bars it plays in: where it is within 25 dB of its loudest (a level is 45 dB to the
    // one), which what bleeds into a stem from the others is not; gaps of under two seconds closed.
    let counted = Counted::new(heard);
    let mut plays: Vec<(f64, f64)> = Vec::new();
    for (index, level) in
        heard.activity.iter().filter(|(name, _)| name == stem).flat_map(|(_, levels)| levels).enumerate()
    {
        let at = index as f64 * 0.1;
        if *level < 0.45 {
            continue;
        }
        match plays.last_mut() {
            Some(last) if at - last.1 < 2.0 => last.1 = at + 0.1,
            _ => plays.push((at, at + 0.1)),
        }
    }
    let mut about = json!({ "name": stem, "path": studied.folder.stem(stem).display().to_string() });
    if !heard.grid.beats.is_empty() {
        about["plays_in_bars"] = plays
            .iter()
            .map(|&(from, to)| json!([counted.place(from).0, counted.place((to - 0.05).max(from)).0]))
            .collect();
    }
    out["stem"] = about;
    Ok(())
}

/// The lists a caller asked for, each to its page.
fn lists(out: &mut Value, heard: &Heard, studied: Option<&Studied>, request: &ListenRequest) {
    let stem = request.stem.as_deref().filter(|_| studied.is_some());
    let drum = |kind: HitKind| kind != HitKind::Bass;
    let counted = Counted::new(heard);
    let (start, end) = counted.stretch(request.from_bar, request.to_bar);
    // What ends on the stretch's first bar line, to a millisecond, is not in it.
    let inside = |from: f64, to: f64| to > start + 1e-3 && from < end - 1e-3;
    let (offset, limit) = (request.offset.unwrap_or(0), request.limit.unwrap_or(DEFAULT_ITEMS).clamp(1, MAX_ITEMS));
    let flats = heard.flats();
    let placed = |seconds: f64| -> Value {
        if heard.grid.beats.is_empty() {
            json!({ "at_seconds": round(seconds) })
        } else {
            let (bar, beat) = counted.place(seconds);
            json!({ "bar": bar, "beat": beat })
        }
    };
    let with = |mut place: Value, more: Value| -> Value {
        for (key, value) in more.as_object().into_iter().flatten() {
            place[key] = value.clone();
        }
        place
    };
    for show in &request.show {
        let (name, items): (&str, Vec<Value>) = match show {
            ListenShow::Chords => (
                "chords",
                heard
                    .harmony
                    .iter()
                    .flat_map(|harmony| &harmony.chords)
                    .filter(|chord| inside(chord.start, chord.end))
                    .map(|chord| with(placed(chord.start), json!({ "beats": counted.beats_between(chord.start, chord.end), "name": chord.name(flats) })))
                    .collect(),
            ),
            ListenShow::Parts => (
                "parts",
                heard
                    .parts
                    .iter()
                    .filter(|part| inside(part.start, part.end))
                    .map(|part| {
                        let mut item = json!({ "label": part.label, "bar": part.bar + 1, "bars": part.bars });
                        if !part.without.is_empty() {
                            item["without"] = json!(part.without);
                        }
                        item
                    })
                    .collect(),
            ),
            ListenShow::Loops => (
                "loops",
                heard
                    .loops
                    .iter()
                    .filter(|found| inside(found.start, found.end))
                    .filter(|found| stem.is_none_or(|stem| found.stems.len() == 1 && found.stems[0] == stem))
                    .map(|found| {
                        let mut item = json!({
                            "id": found.id,
                            "of": found.label,
                            "bar": found.bar + 1,
                            "bars": found.bars,
                            "bpm": found.bpm,
                            "score": found.score,
                            "comes_round": found.rounds,
                            "from_seconds": found.start,
                            "to_seconds": found.end,
                        });
                        if !found.part.is_empty() {
                            item["part"] = json!(found.part);
                        }
                        if found.solo {
                            item["plays_alone"] = json!(true);
                        }
                        item
                    })
                    .collect(),
            ),
            ListenShow::OneShots => (
                "one_shots",
                heard
                    .one_shots
                    .iter()
                    .filter(|shot| inside(shot.start, shot.end))
                    .filter(|shot| match stem {
                        None => true,
                        Some("drums") => drum(shot.kind),
                        Some("bass") => !drum(shot.kind),
                        Some(_) => false,
                    })
                    .map(|shot| {
                        let mut item = json!({
                            "id": shot.id,
                            "label": shot.label,
                            "from_seconds": shot.start,
                            "to_seconds": shot.end,
                            "score": shot.score,
                        });
                        if let Some(note) = shot.note {
                            item["pitch"] = json!(note);
                        }
                        item
                    })
                    .collect(),
            ),
            ListenShow::Groove => ("groove", groove_by_bar(heard, &counted, start, end)),
            ListenShow::Bass | ListenShow::Melody => {
                let (name, notes) = if *show == ListenShow::Bass { ("bass", &heard.bass) } else { ("melody", &heard.melody) };
                let items = notes
                    .iter()
                    .filter(|note| inside(note.start, note.end))
                    .map(|note| {
                        with(placed(note.start), json!({ "beats": counted.beats_between(note.start, note.end), "note": note_name(note.midi, flats), "pitch": note.midi }))
                    })
                    .collect();
                (name, items)
            }
            ListenShow::Bars => {
                let lines = heard.grid.bar_times(heard.seconds);
                let items = lines
                    .windows(2)
                    .enumerate()
                    .filter(|(_, pair)| inside(pair[0], pair[1]))
                    .map(|(index, pair)| {
                        json!({
                            "bar": index + 1,
                            "at_seconds": round(pair[0]),
                            "bpm": ((60.0 * counted.per_bar / (pair[1] - pair[0]).max(1e-9)) * 100.0).round() / 100.0,
                        })
                    })
                    .collect();
                ("bars", items)
            }
            ListenShow::Words => {
                let Some(lyrics) = studied.and_then(|studied| studied.lyrics.as_ref()) else {
                    out["words"] = json!(match studied {
                        Some(_) => "this study wrote down no words: none were asked for, or nobody sings",
                        None => "the words are written down by a study (the study tool), which splits the voice from the rest first: listen with its `study`",
                    });
                    continue;
                };
                let items = lyrics
                    .lines
                    .iter()
                    .filter(|line| inside(line.start, line.end))
                    .map(|line| {
                        let words: Vec<Value> = line
                            .words
                            .iter()
                            .map(|word| {
                                if heard.grid.beats.is_empty() {
                                    json!([round(word.start), word.text])
                                } else {
                                    let (bar, beat) = counted.place(word.start);
                                    json!([bar, beat, word.text])
                                }
                            })
                            .collect();
                        with(placed(line.start), json!({ "beats": counted.beats_between(line.start, line.end), "text": line.text, "words": words }))
                    })
                    .collect();
                ("lines", items)
            }
        };
        out[name] = page(items, offset, limit);
    }
    let of_a_mix = request
        .show
        .iter()
        .any(|show| matches!(show, ListenShow::Groove | ListenShow::Bass | ListenShow::Melody | ListenShow::OneShots));
    if of_a_mix && heard.stems.is_empty() {
        out["heard_as"] = json!(
            "one sound, not taken apart: right for a drum loop, a bass line or a voice alone. Of a whole mix the \
             groove is a sketch (the bass plays in the kick's band, other instruments in the snare's), the bass is \
             what is loudest under 260 Hz, and the melody what is loudest above."
        );
    }
}

/// The drum part bar by bar: for each drum, the beats of the bar it is
/// struck on, to the sixteenth.
fn groove_by_bar(heard: &Heard, counted: &Counted<'_>, start: f64, end: f64) -> Vec<Value> {
    let mut bars: Vec<(i64, [Vec<f64>; 3])> = Vec::new();
    for note in heard.groove.iter().filter(|note| note.time >= start && note.time < end) {
        let (bar, beat) = if heard.grid.beats.is_empty() { (1, note.time) } else { counted.place(note.time) };
        // To the sixteenth; one that rounds up to the next bar line is that bar's first.
        let mut beat = (beat * 4.0).round() / 4.0;
        let mut bar = bar;
        if !heard.grid.beats.is_empty() && beat >= counted.per_bar + 1.0 {
            beat -= counted.per_bar;
            bar += 1;
        }
        let drum = match note.kind {
            HitKind::Kick => 0,
            HitKind::Snare => 1,
            _ => 2,
        };
        if bars.last().is_none_or(|last| last.0 != bar) {
            bars.push((bar, Default::default()));
        }
        let slot = &mut bars.last_mut().expect("just pushed").1[drum];
        if slot.last() != Some(&beat) {
            slot.push(beat);
        }
    }
    bars.into_iter()
        .map(|(bar, [kick, snare, hat])| json!({ "bar": bar, "kick": kick, "snare": snare, "hat": hat }))
        .collect()
}

impl TrillServer {
    /// What a request listens to: a file by its path, the file of an audio clip, or a study.
    fn sound_of(&self, path: Option<&str>, clip: Option<u64>, study: Option<&str>) -> Result<Sound, String> {
        let study = study.map(str::trim).filter(|study| !study.is_empty());
        match (path.filter(|path| !path.trim().is_empty()), clip, study) {
            (Some(path), None, None) => Ok(Sound::File(path.to_owned())),
            (None, Some(clip), None) => {
                let session = self.session.lock();
                let project = session.project();
                let found = project.clip(ClipId(clip)).map_err(|error| explain(project, error))?;
                found.audio().map(|audio| Sound::File(audio.path.clone())).ok_or_else(|| {
                    format!(
                        "clip {clip} ({}) holds notes, not audio: there is no recording in it to listen to",
                        found.name
                    )
                })
            }
            (None, None, Some(study)) => Ok(Sound::Study(study.to_owned())),
            (None, None, None) => Err(
                "say what to listen to: `path` (an audio file), `clip` (an audio clip of the song) or `study` (a study's id)"
                    .to_owned(),
            ),
            _ => Err("give one of `path`, `clip` and `study`, not more".to_owned()),
        }
    }

    /// Listens to a file, or reads a study, off the threads that serve the
    /// other calls. Corrections given for a study are kept with it, and it
    /// is heard again over them first.
    async fn heard(&self, sound: &Sound, grid: &[String]) -> Result<(Arc<Heard>, Option<Studied>), String> {
        let runtime = self.runtime.clone();
        match sound {
            Sound::File(path) => {
                let corrections = corrections(grid)?;
                let file = PathBuf::from(path);
                let heard = tokio::task::spawn_blocking(move || {
                    runtime.hear(&file, &corrections).map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| format!("listening stopped: {error}"))??;
                Ok((heard, None))
            }
            Sound::Study(id) => {
                let wanted = match grid {
                    [] => None,
                    [none] if none.trim() == "none" => Some(Vec::new()),
                    grid => Some(corrections(grid)?),
                };
                let id = id.clone();
                tokio::task::spawn_blocking(move || {
                    let desk = runtime.studies()?;
                    if let Some(wanted) = wanted
                        && desk.report(&id)?.study.corrections != wanted
                    {
                        desk.correct(&id, wanted)?;
                        desk.wait(&id, Duration::from_secs(300))?;
                    }
                    let studied = runtime.study(&id)?;
                    match studied.heard.clone() {
                        Some(heard) if studied.report.study.state == trill_study::State::Done => {
                            Ok((heard, Some(studied)))
                        }
                        _ => Err(not_done(&studied.report)),
                    }
                })
                .await
                .map_err(|error| format!("reading the study stopped: {error}"))?
            }
        }
    }

    async fn transcribed(&self, request: TranscribeRequest) -> Reply {
        if request.file.is_none() && request.at.is_none() {
            return Err(
                "say where it goes: `file` to write a MIDI file, or `at` (a beat of the song) to put it on a new track"
                    .to_owned(),
            );
        }
        let sound = self.sound_of(request.path.as_deref(), request.clip, request.study.as_deref())?;
        let (heard, _) = self.heard(&sound, &request.grid).await?;
        let counted = Counted::new(&heard);
        // From the first bar line, so that the recording's bars are the file's.
        let from_bar = request.from_bar.or((!heard.grid.beats.is_empty()).then_some(1));
        let (start, end) = counted.stretch(from_bar, request.to_bar);
        let part = match request.part {
            TranscribePart::Bass => MidiPart::Bass,
            TranscribePart::Melody => MidiPart::Melody,
            TranscribePart::Groove => MidiPart::Groove,
            TranscribePart::Chords => MidiPart::Chords,
        };
        let file = heard.midi(part, start, end, request.quantize.map(|steps| steps.min(48)));
        let notes = file.tracks.first().map_or(0, |track| track.notes.len());
        if notes == 0 {
            return Err(format!(
                "no {} was heard there: listen says what was found in this file (`found`), and of which bars",
                part.name().to_lowercase()
            ));
        }
        let mut out = json!({
            "part": part.name(),
            "notes": notes,
            "beats": round(file.length_beats()),
            "tempo": file.tempo,
        });
        let mut session = self.session.lock();
        if let Some(requested) = request.file.as_deref() {
            let target = self
                .write_path(&session, Some(requested), "a MIDI file", &["mid", "midi"])?
                .unwrap_or_else(|| PathBuf::from(requested));
            if let Some(parent) = target.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
            }
            std::fs::write(&target, trill_core::midi_file::write(&file))
                .map_err(|error| format!("{}: {error}", target.display()))?;
            out["file"] = json!(target.display().to_string());
        }
        if let Some(at) = request.at {
            let melodic = request.instrument.as_deref().unwrap_or("builtin:polysynth");
            if !session.factory().descriptors().iter().any(|descriptor| descriptor.uri == melodic) {
                return Err(format!("unknown device '{melodic}'; list_devices shows what is available"));
            }
            out["tracks"] = json!(import_parts(&mut session, &file, at, false, melodic)?);
            out["song_tempo"] = json!(session.project().tempo);
        }
        Ok(out)
    }
}

#[tool_router(router = listening_router, vis = "pub(crate)")]
impl TrillServer {
    /// Say what is in an audio file, without changing anything and without a model: its tempo and bars, key,
    /// tuning, and how many chords, parts, loops, single hits and notes were found; and, with `show`, those
    /// lists themselves, placed in the recording's own bars. Use it to read a reference, or to learn the tempo
    /// and key of a sound whose name does not say. Given a `study` it reads what a study found in the
    /// recording's stems, with the words.
    #[tool(annotations(title = "Listen to a sound", read_only_hint = true))]
    async fn listen(&self, Parameters(request): Parameters<ListenRequest>) -> CallToolResult {
        let reply = async {
            let sound = self.sound_of(request.path.as_deref(), request.clip, request.study.as_deref())?;
            let (heard, studied) = self.heard(&sound, &request.grid).await?;
            let mut out = summary(&heard);
            match (&sound, &studied) {
                (Sound::File(path), _) => {
                    if request.stem.is_some() {
                        return Err("`stem` is of a study: a file is listened to as one sound".to_owned());
                    }
                    out["file"] = json!(path);
                }
                (Sound::Study(_), Some(studied)) => of_study(&mut out, &heard, studied, request.stem.as_deref())?,
                (Sound::Study(_), None) => {}
            }
            lists(&mut out, &heard, studied.as_ref(), &request);
            Ok(out)
        }
        .await;
        finish(reply)
    }

    /// Write what was heard in an audio file as MIDI: its bass line, its melody, its drum groove or its chords,
    /// placed by the recording's own beats. Into a MIDI file (`file`), or into the song on a new track from
    /// `at` beats on.
    #[tool(annotations(title = "Transcribe a sound to MIDI"))]
    async fn transcribe(&self, Parameters(request): Parameters<TranscribeRequest>) -> CallToolResult {
        finish(self.transcribed(request).await)
    }
}
