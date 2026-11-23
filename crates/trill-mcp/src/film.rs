//! Tools for filming the song: the stage, as a file.
//!
//! * `film_plan` says what a film would be before any of it is made: the
//!   takes the director would shoot (or, of a making-of, the shots of the
//!   story), with what was asked of it applied. It costs nothing.
//! * `film` makes it: the song rendered for its own sound, every frame
//!   drawn at its exact time, the two put together by `ffmpeg`. That takes
//!   minutes, which is why the plan can be read first.
//!
//! The stage, its director and the film itself are `trill-stage`'s. What
//! is here is the arguments turned into what that crate takes (a section's
//! name into beats, track ids into parts), errors that say what does
//! exist, and replies short enough to read.

use crate::requests::{FilmDirection, FilmEntry, FilmOf, FilmPlanRequest, FilmQuality, FilmRequest, FilmShot};
use crate::server::{Reply, TrillServer, finish};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use trill_core::{Beats, MeterMap, Project, TrackId};
use trill_engine::Session;
use trill_stage::director::{Direction, Entry, Kind, Plan};
use trill_stage::export::{Exported, FilmRequest as SongRequest, SongFilm};
use trill_stage::film::{Progress, Shoot};
use trill_stage::making::{Making, MakingFilm, MakingRequest};
use trill_stage::scene::shot_word;
use trill_stage::{Format, Score};

/// Appended to the server's instructions.
pub(crate) const INSTRUCTIONS: &str = "\
Filming the song
Trill can film a song: its own lanes, clips and piano rolls on a stage, lit by what plays, with the song's own \
sound, as an .mp4. A director chooses the shots from what the song is (wide on its fullest sections, close on \
the piano roll of a part that carries the tune, the lanes with the rolls of who drives a section otherwise; a \
cut where the song jumps, a move where it does not).
- film_plan says what would be filmed, take by take, and costs nothing. Read it before film, which takes \
minutes: about twice the length of the song at 1920x1080 and eight times at 3840x2160, its sound rendered first \
(plugins are waited for). Where a call may only take so long, film at 1920x1080, or a section at a time.
- film makes the file. `size` is \"1080p\", \"2160p\" or \"upright\"; `section` or `start`/`end` film a stretch.
- To direct, give `direct`: a list of stretches (a `section` by name, or `from_bar`/`to_bar`) each with a `shot` \
(\"wide\", \"close\", \"ensemble\", \"song\"), the `parts` to show as piano rolls, or how to `enter` it (\"cut\", \
\"move\"). What you leave out stays the director's. Check the result with film_plan.
- `of` \"making_of\" films how the song was made instead: the journal played again, the camera where each step \
landed, what was asked and answered as cards, the finished song playing under it. It needs a journal with steps \
in it (see journal).
- A film needs a graphics card, and ffmpeg on the PATH to become one file; without ffmpeg the frames and the \
sound are written as files and the reply says so.";

/// A number to the tenth, which is as fine as a reply about a film needs.
fn tenths(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// The sides a film may have.
const SIDES: std::ops::RangeInclusive<u32> = 320..=3840;
/// The rates a film may have.
const RATES: std::ops::RangeInclusive<u32> = 24..=60;

/// The size a film was asked to be.
fn size_of(asked: Option<&str>) -> Result<Format, String> {
    let named = match asked.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        None | Some("1080p" | "hd") => Some(Format::HD),
        Some("2160p" | "4k" | "uhd") => Some(Format::UHD),
        Some("upright" | "portrait" | "vertical") => Some(Format::UPRIGHT),
        Some(_) => None,
    };
    if let Some(format) = named {
        return Ok(format);
    }
    let asked = asked.unwrap_or_default();
    let sides = asked
        .split_once(['x', 'X'])
        .and_then(|(width, height)| Some((width.trim().parse::<u32>().ok()?, height.trim().parse::<u32>().ok()?)));
    match sides {
        Some((width, height))
            if SIDES.contains(&width) && SIDES.contains(&height) && width % 2 == 0 && height % 2 == 0 =>
        {
            Ok(Format::new(width, height))
        }
        _ => Err(format!(
            "`size` '{asked}' is not a size a film can have: give \"1080p\" (1920x1080), \"2160p\" (3840x2160), \
             \"upright\" (1080x1920) or WIDTHxHEIGHT with even sides from {} to {}",
            SIDES.start(),
            SIDES.end()
        )),
    }
}

/// How a film is shot, from what was asked.
fn shoot_of(request: &FilmRequest) -> Result<Shoot, String> {
    let format = size_of(request.size.as_deref())?;
    let rate = request.rate.unwrap_or(60);
    if !RATES.contains(&rate) {
        return Err(format!("`rate` {rate} is outside {} to {} frames a second", RATES.start(), RATES.end()));
    }
    Ok(match request.quality {
        FilmQuality::Fine => Shoot { rate, ..Shoot::fine(format) },
        FilmQuality::Plain => Shoot { rate, ..Shoot::plain(format) },
    })
}

/// The sections of `score` that have names, for an error to list.
fn section_names(score: &Score) -> String {
    let names: Vec<String> = score
        .sections
        .iter()
        .filter(|section| !section.name.is_empty())
        .map(|section| format!("\"{}\"", section.name))
        .collect();
    match names.is_empty() {
        true => "the song has no markers, so no sections by name: give bars instead".to_string(),
        false => format!("the song's sections are {}", names.join(", ")),
    }
}

/// The section of `score` called `name`, however it is capitalized.
fn section_of(score: &Score, name: &str) -> Result<(Beats, Beats), String> {
    let wanted = name.trim().to_lowercase();
    let found = score.sections.iter().find(|section| !wanted.is_empty() && section.name.to_lowercase() == wanted);
    found
        .map(|section| (section.start, section.end))
        .ok_or_else(|| format!("no section is called '{name}': {}", section_names(score)))
}

/// The stretch of the song a film was asked to be of: from, and to
/// (`None` for the end).
fn stretch_of(score: &Score, request: &FilmPlanRequest) -> Result<(Beats, Option<Beats>), String> {
    if let Some(name) = &request.section {
        if request.start.is_some() || request.end.is_some() {
            return Err("give `section`, or `start` and `end`, not both".to_string());
        }
        return section_of(score, name).map(|(from, to)| (from, Some(to)));
    }
    let from = request.start.unwrap_or(0.0);
    let to = request.end.unwrap_or(score.length);
    if !from.is_finite() || !to.is_finite() || from < 0.0 || to <= from || from >= score.length {
        return Err(format!(
            "the stretch {from}..{to} beats is not in the song, which is {} beats ({} bars) long",
            score.length,
            score.bar_count()
        ));
    }
    Ok((from, request.end))
}

/// What was asked of the director, as the director takes it.
fn directions_of(project: &Project, score: &Score, asked: &[FilmDirection]) -> Result<Vec<Direction>, String> {
    // Bars are the song's own: through a change of time signature they
    // are counted as the song counts them.
    let bars = f64::from(score.bar_count());
    let mut directions = Vec::with_capacity(asked.len());
    for (index, direction) in asked.iter().enumerate() {
        let place = format!("direct[{index}]");
        let (from, to) = match (&direction.section, direction.from_bar) {
            (Some(_), Some(_)) => return Err(format!("{place}: give `section` or `from_bar`, not both")),
            (Some(name), None) => section_of(score, name).map_err(|error| format!("{place}: {error}"))?,
            (None, Some(first)) => {
                let last = direction.to_bar.unwrap_or(first.floor());
                if !first.is_finite() || !last.is_finite() || first < 1.0 || last < first.floor() || first > bars {
                    return Err(format!(
                        "{place}: bars {first} to {last} are not in the song, which has bars 1 to {bars}"
                    ));
                }
                (score.bars.beat_of_bars(first - 1.0), score.bars.bar_start(last.floor() as u32).min(score.length))
            }
            (None, None) => return Err(format!("{place} names no stretch: give `section` or `from_bar`")),
        };
        let mut rolls = Vec::new();
        for id in direction.parts.iter().flatten() {
            let track = project.track(TrackId(*id)).map_err(|error| crate::server::explain(project, error))?;
            if score.part(track.id).is_none() {
                return Err(format!(
                    "{place}: track {id} ({}) has no clips to show; the parts of the song are {}",
                    track.name,
                    parts_listed(score)
                ));
            }
            rolls.push(track.id);
        }
        let kind = direction.shot.map(|shot| match shot {
            FilmShot::Song => Kind::Overview,
            FilmShot::Wide => Kind::Wide,
            FilmShot::Ensemble => Kind::Ensemble,
            FilmShot::Close => Kind::Close,
        });
        let entry = direction.enter.map(|entry| match entry {
            FilmEntry::Cut => Entry::Cut,
            FilmEntry::Move => Entry::Move(score.bar_length(from)),
        });
        if kind.is_none() && rolls.is_empty() && entry.is_none() {
            return Err(format!("{place} asks for nothing: give `shot`, `parts` or `enter`"));
        }
        directions.push(Direction { from, to, kind, rolls, entry });
    }
    Ok(directions)
}

/// The parts of `score` with their track ids, for an error to list.
fn parts_listed(score: &Score) -> String {
    let parts: Vec<String> = score.parts.iter().map(|part| format!("{} ({})", part.track.0, part.name)).collect();
    parts.join(", ")
}

/// A span of beats as bars counted from 1, as the song counts them:
/// "5-12", or "5" for one bar.
fn bars_text(from: Beats, to: Beats, bars: &MeterMap) -> String {
    let first = bars.bar_at(from).number();
    let last = bars.bar_at((to - 1e-6).max(from)).number().max(first);
    if first == last { first.to_string() } else { format!("{first}-{last}") }
}

/// The plan of a film of the song, as a reply: its takes within the
/// stretch, each with where it is, how it is framed and how it is entered.
fn plan_json(score: &Score, plan: &Plan, from: Beats, to: Beats) -> Value {
    let bar = &score.bars;
    let takes: Vec<Value> = plan
        .takes
        .iter()
        .filter(|take| take.end > from && take.start < to)
        .map(|take| {
            let parts: Vec<&str> =
                take.shot.rolls.iter().filter_map(|track| score.part(*track)).map(|part| part.name.as_str()).collect();
            let mut out = json!({
                "bars": bars_text(take.start.max(from), take.end.min(to), bar),
                "at_seconds": tenths(score.time.between(from, take.start.max(from))),
                "shot": shot_word(take.shot.kind).to_lowercase(),
                "enter": match take.entry {
                    Entry::Cut => "cut",
                    Entry::Move(beats) if beats > 0.0 => "move",
                    Entry::Move(_) => "carries on",
                },
            });
            if let Some(section) = score.sections.get(take.section).filter(|section| !section.name.is_empty()) {
                out["section"] = json!(section.name);
            }
            if !parts.is_empty() {
                out["parts"] = json!(parts);
            }
            out
        })
        .collect();
    // Where the tempo or the time signature changes in the stretch: the
    // picture says so as it arrives, and the bars and seconds here are
    // counted through it.
    let changes: Vec<Value> = score
        .changes
        .iter()
        .filter(|change| change.beat > from - 1e-9 && change.beat < to)
        .map(|change| {
            json!({
                "bar": score.bar_number(change.beat),
                "at_seconds": tenths(score.time.between(from, change.beat)),
                "to": change.said(),
            })
        })
        .collect();
    let mut out = json!({
        "of": "song",
        "bars": bars_text(from, to, bar),
        "seconds": tenths(score.time.between(from, to)),
        "takes": takes,
        "parts": score.parts.iter().map(|part| json!({ "track": part.track.0, "name": part.name })).collect::<Vec<_>>(),
        "direct": "a list of {section | from_bar, to_bar} with shot (wide, close, ensemble, song), parts (track \
                   ids) and enter (cut, move) changes these takes; what is left out stays as it is here",
    });
    if !changes.is_empty() {
        out["tempo_and_meter"] = json!(changes);
    }
    out
}

/// The plan of a film of how the song was made.
fn making_json(making: &Making, project: &Project) -> Value {
    let name_of = |track: TrackId| project.track(track).map(|track| track.name.clone()).unwrap_or_default();
    let bar = &project.meter_map();
    let shots: Vec<Value> = making
        .shots()
        .iter()
        .map(|shot| {
            let mut out = json!({
                "at_seconds": tenths(making.film_time(shot.from)),
                "bars": bars_text(shot.span.0.max(0.0), shot.span.1, bar),
                "shot": if shot.close.is_some() { "close" } else { "wide" },
            });
            if let Some(track) = shot.close {
                out["part"] = json!(name_of(track));
            }
            out
        })
        .collect();
    let mut out = json!({
        "of": "making_of",
        "steps": making.steps(),
        "seconds": tenths(making.length()),
        "story_seconds": tenths(making.story_length()),
        "shots": shots.len(),
        "first_shots": shots.iter().take(24).collect::<Vec<_>>(),
        "how": "the camera goes where each step landed (close on a part being written, wide otherwise); what \
                was asked and answered are cards; it ends on the finished song. `speed` makes it shorter",
    });
    if let Some(broken) = making.broken() {
        out["broken"] = json!(format!("the journal cannot be followed to its end ({broken}); the film stops there"));
    }
    out
}

/// Where a film goes when no place is named: a `films` folder beside the
/// project, or the system's temporary folder.
fn default_film_path(session: &Session, of: FilmOf) -> PathBuf {
    let kept = |c: char| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' };
    let name: String = session.project().name.chars().map(kept).collect();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let folder = match session.path().and_then(Path::parent) {
        Some(parent) => parent.join("films"),
        None => std::env::temp_dir().join("trill-films"),
    };
    let what = if of == FilmOf::MakingOf { "-making-of" } else { "" };
    folder.join(format!("{name}{what}-{stamp}.mp4"))
}

/// What came of a film, as a reply.
fn exported_json(exported: &Exported, shoot: &Shoot, of: FilmOf) -> Value {
    let mut out = json!({
        "path": exported.path.display().to_string(),
        "of": if of == FilmOf::MakingOf { "making_of" } else { "song" },
        "seconds": tenths(exported.seconds),
        "frames": exported.frames,
        "size": format!("{}x{}", shoot.format.width, shoot.format.height),
        "rate": shoot.rate,
        "took_seconds": tenths(exported.took),
    });
    if let (Some(loudness), Some(peak)) = (exported.loudness_lufs, exported.peak_db) {
        out["loudness_lufs"] = json!(tenths(f64::from(loudness)));
        out["peak_db"] = json!(tenths(f64::from(peak)));
    }
    if !exported.notes.is_empty() {
        out["notes"] = json!(exported.notes);
    }
    out
}

/// The tracks of the session's song that play through a drum instrument.
fn drum_tracks(session: &Session) -> Vec<TrackId> {
    let tracks = session.project().tracks.iter().map(|track| track.id);
    tracks.filter(|track| session.instrument_plays_drums(*track)).collect()
}

/// A film that is ready to be made, or looked at.
enum Planned {
    Song(Box<SongFilm>),
    Making(Box<MakingFilm>),
}

#[tool_router(router = film_router, vis = "pub(crate)")]
impl TrillServer {
    /// What a film of the song would be, WITHOUT making it: the takes the director would shoot, in order, each
    /// with its bars, its shot (wide, close, ensemble, song), the parts shown as piano rolls and whether it is
    /// cut or moved to. With `direct`, the plan with what you asked applied: read it to see whether a direction
    /// did what you meant before spending minutes on `film`. With `of` "making_of", how the journal would be
    /// filmed: how many steps, how long, where the camera goes.
    #[tool(annotations(title = "Plan a film", read_only_hint = true))]
    fn film_plan(&self, Parameters(request): Parameters<FilmPlanRequest>) -> CallToolResult {
        let reply = (|| -> Reply {
            let request = FilmRequest { what: request, ..FilmRequest::default() };
            let path = PathBuf::from("plan.mp4");
            match self.planned(&request, path)? {
                Planned::Song(film) => {
                    let song = film.song();
                    let (from, to) = film.stretch();
                    Ok(plan_json(&song.score, &song.plan, from, to))
                }
                Planned::Making(film) => Ok(making_json(&film.making()?, &film.render.project)),
            }
        })();
        finish(reply)
    }

    /// Film the song: an .mp4 of its lanes, clips and piano rolls on the stage, shot by a director, lit by what
    /// plays, with the song's own sound rendered in the same job (plugins are waited for, as `render` waits).
    /// Takes minutes: read `film_plan` first. `direct` changes the shots; `section` or `start`/`end` film a
    /// stretch; `size` "upright" is for phones. With `of` "making_of" it films how the song was made instead.
    /// Needs a graphics card; without ffmpeg on the PATH the frames and the sound are written as files.
    #[tool(annotations(title = "Film the song"))]
    async fn film(&self, Parameters(request): Parameters<FilmRequest>) -> CallToolResult {
        let of = request.what.of;
        // Everything is taken from the session under the lock; the film is
        // made outside it, as a render is.
        let prepared = (|| -> Result<(Planned, Shoot), String> {
            let shoot = shoot_of(&request)?;
            let path = {
                let session = self.session.lock();
                let usual = || default_film_path(&session, of);
                self.write_path_or(&session, request.path.as_deref(), "a film", &["mp4"], "films", usual)?
            };
            Ok((self.planned(&request, path)?, shoot))
        })();
        let (planned, shoot) = match prepared {
            Ok(prepared) => prepared,
            Err(refusal) => return finish(Err(refusal)),
        };
        let outcome = tokio::task::spawn_blocking(move || {
            let progress = Progress::default();
            match planned {
                Planned::Song(film) => film.run(&progress),
                Planned::Making(film) => film.run(&progress),
            }
        })
        .await;
        finish(match outcome {
            Ok(Ok(exported)) => Ok(exported_json(&exported, &shoot, of)),
            Ok(Err(sentence)) => Err(sentence),
            Err(error) => Err(format!("the film did not finish: {error}")),
        })
    }
}

impl TrillServer {
    /// The film `request` asks for, to be written to `path`, with
    /// everything it needs taken from the session now.
    fn planned(&self, request: &FilmRequest, path: PathBuf) -> Result<Planned, String> {
        let shoot = shoot_of(request)?;
        let (render, drums, sample_rate, journal) = {
            let mut session = self.session.lock();
            let journal = session.journal().filter(|journal| journal.steps() > 0).cloned();
            let drums = drum_tracks(&session);
            let sample_rate = session.audio_info().sample_rate as u32;
            (session.render_job(), drums, sample_rate, journal)
        };
        let project = &render.project;
        let what = &request.what;
        match what.of {
            FilmOf::Song => {
                if project.length_beats() <= 0.0 {
                    return Err("the song is empty: there is nothing to film yet".to_string());
                }
                if what.speed.is_some() || request.music.is_some() {
                    return Err("`speed` and `music` are for a film `of` \"making_of\"".to_string());
                }
                let score = Score::of(project, |track| drums.contains(&track.id));
                let (from, to) = stretch_of(&score, what)?;
                let directions = directions_of(project, &score, what.direct.as_deref().unwrap_or_default())?;
                let request = SongRequest { from, to, directions, sample_rate, ..SongRequest::new(path, shoot) };
                Ok(Planned::Song(Box::new(SongFilm { render, drums, request })))
            }
            FilmOf::MakingOf => {
                if what.section.is_some() || what.start.is_some() || what.end.is_some() || what.direct.is_some() {
                    return Err("a making-of is filmed whole and directs itself: `section`, `start`, `end` and \
                                `direct` are for a film of the song"
                        .to_string());
                }
                if what.speed.is_some_and(|speed| !speed.is_finite() || !(0.1..=64.0).contains(&speed)) {
                    return Err("`speed` is how many seconds of the story a second of film is: 0.1 to 64".to_string());
                }
                let journal = journal.ok_or_else(|| {
                    "there is no journal with steps in it to film: one is kept from when `journal` is told to \
                     \"keep\" (the default), and holds what was made since"
                        .to_string()
                })?;
                let request = MakingRequest {
                    speed: what.speed,
                    music: request.music.unwrap_or(true),
                    sample_rate,
                    ..MakingRequest::new(path, shoot)
                };
                Ok(Planned::Making(Box::new(MakingFilm { journal, render, drums, request })))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::Note;

    /// Sixteen bars in two sections, a bass throughout and a lead in the second.
    fn song() -> (Project, TrackId, TrackId) {
        let mut project = Project::new("Filmed");
        let line = |pitch: u8, beats: u32| -> Vec<Note> {
            (0..beats * 2)
                .map(|n| Note {
                    pitch: pitch + (n % 5) as u8,
                    start: f64::from(n) * 0.5,
                    length: 0.4,
                    velocity: 100,
                    ..Default::default()
                })
                .collect()
        };
        let bass = project.add_track("Bass");
        let clip = project.add_midi_clip(bass, "bass", 0.0, 64.0).unwrap();
        project.clip_mut(clip).unwrap().add_notes(&line(36, 64)).unwrap();
        let lead = project.add_track("Lead");
        let clip = project.add_midi_clip(lead, "lead", 32.0, 32.0).unwrap();
        project.clip_mut(clip).unwrap().add_notes(&line(72, 32)).unwrap();
        project.add_track("Empty");
        project.add_marker(0.0, "Verse").unwrap();
        project.add_marker(32.0, "Chorus").unwrap();
        (project, bass, lead)
    }

    #[test]
    fn a_film_given_no_place_goes_where_films_go_and_the_assistants_into_its_working_folder() {
        use trill_runtime::{Runtime, RuntimeOptions};
        let folder = tempfile::tempdir().unwrap();
        let work = folder.path().join("work");
        let runtime = std::sync::Arc::new(Runtime::start(RuntimeOptions::hermetic()).unwrap());
        let usual = || PathBuf::from("somewhere").join("else").join("Song-17.mp4");
        let place = |server: &TrillServer, asked: Option<&str>| {
            server.write_path_or(&runtime.session.lock(), asked, "a film", &["mp4"], "films", usual)
        };
        // A place of the machine's outside the assistant's folder: on another drive, or at the root.
        let elsewhere = if cfg!(windows) { "D:/films/night.mp4" } else { "/films/night.mp4" };
        // A client trusted as the user is: where it says, or where films go.
        let trusted = TrillServer::new(runtime.clone());
        assert_eq!(place(&trusted, None), Ok(usual()));
        assert_eq!(place(&trusted, Some(elsewhere)), Ok(PathBuf::from(elsewhere)));
        // The assistant: under the same name, in `films` in its working folder.
        let guarded = TrillServer::guarded(runtime.clone(), work.clone());
        let kept = place(&guarded, None).unwrap();
        assert_eq!(kept.file_name().and_then(|name| name.to_str()), Some("Song-17.mp4"));
        let films = kept.parent().unwrap();
        assert_eq!(films.file_name().and_then(|name| name.to_str()), Some("films"));
        assert_eq!(films.parent().unwrap().canonicalize().unwrap(), work.canonicalize().unwrap());
        // And where it does say, only there.
        let refusal = place(&guarded, Some(elsewhere)).unwrap_err();
        assert!(refusal.contains("a film can only be written inside"), "{refusal}");
    }

    #[test]
    fn a_size_is_a_name_or_two_even_sides() {
        assert_eq!(size_of(None).unwrap(), Format::HD);
        assert_eq!(size_of(Some("2160p")).unwrap(), Format::UHD);
        assert_eq!(size_of(Some(" Upright ")).unwrap(), Format::UPRIGHT);
        assert_eq!(size_of(Some("1280x720")).unwrap(), Format::new(1280, 720));
        for wrong in ["huge", "1281x720", "100x100", "9000x4000", "1280"] {
            let refusal = size_of(Some(wrong)).unwrap_err();
            assert!(refusal.contains(wrong) && refusal.contains("\"upright\""), "{refusal}");
        }
        let fast = FilmRequest { rate: Some(120), ..Default::default() };
        assert!(shoot_of(&fast).unwrap_err().contains("24 to 60"));
        let plain = FilmRequest { quality: FilmQuality::Plain, rate: Some(30), ..Default::default() };
        assert_eq!(shoot_of(&plain).unwrap(), Shoot { rate: 30, ..Shoot::plain(Format::HD) });
    }

    #[test]
    fn a_stretch_is_a_section_by_name_or_beats_inside_the_song() {
        let (project, ..) = song();
        let score = Score::read(&project);
        let asked = |section: Option<&str>, start, end| FilmPlanRequest {
            section: section.map(str::to_string),
            start,
            end,
            ..Default::default()
        };
        assert_eq!(stretch_of(&score, &asked(None, None, None)).unwrap(), (0.0, None));
        assert_eq!(stretch_of(&score, &asked(Some("chorus"), None, None)).unwrap(), (32.0, Some(64.0)));
        assert_eq!(stretch_of(&score, &asked(None, Some(8.0), Some(24.0))).unwrap(), (8.0, Some(24.0)));
        let unknown = stretch_of(&score, &asked(Some("Bridge"), None, None)).unwrap_err();
        assert_eq!(unknown, "no section is called 'Bridge': the song's sections are \"Verse\", \"Chorus\"");
        assert!(stretch_of(&score, &asked(Some("Verse"), Some(0.0), None)).unwrap_err().contains("not both"));
        let outside = stretch_of(&score, &asked(None, Some(80.0), None)).unwrap_err();
        assert!(outside.contains("64 beats (16 bars)"), "{outside}");
    }

    #[test]
    fn directions_name_sections_bars_and_tracks_and_mistakes_say_what_exists() {
        let (project, bass, lead) = song();
        let score = Score::read(&project);
        let direct = |direction: FilmDirection| directions_of(&project, &score, &[direction]);
        let close =
            direct(FilmDirection { section: Some("Chorus".into()), parts: Some(vec![lead.0]), ..Default::default() });
        assert_eq!(close.unwrap(), [Direction { from: 32.0, to: 64.0, kind: None, rolls: vec![lead], entry: None }]);
        // Bars are counted from 1, and `to_bar` is the last bar filmed.
        let wide = direct(FilmDirection {
            from_bar: Some(3.0),
            to_bar: Some(4.0),
            shot: Some(FilmShot::Wide),
            enter: Some(FilmEntry::Move),
            ..Default::default()
        });
        let moved = Some(Entry::Move(4.0));
        assert_eq!(
            wide.unwrap(),
            [Direction { from: 8.0, to: 16.0, kind: Some(Kind::Wide), rolls: vec![], entry: moved }]
        );

        let nothing = direct(FilmDirection { section: Some("Verse".into()), ..Default::default() }).unwrap_err();
        assert!(nothing.contains("direct[0] asks for nothing"), "{nothing}");
        let nowhere = direct(FilmDirection { shot: Some(FilmShot::Close), ..Default::default() }).unwrap_err();
        assert!(nowhere.contains("names no stretch"), "{nowhere}");
        let beyond = direct(FilmDirection { from_bar: Some(40.0), shot: Some(FilmShot::Wide), ..Default::default() });
        assert!(beyond.unwrap_err().contains("bars 1 to 16"));
        // A track with nothing on it cannot be shown, and the reply says which can.
        let empty = project.tracks[2].id.0;
        let bare = direct(FilmDirection { from_bar: Some(1.0), parts: Some(vec![empty]), ..Default::default() });
        let bare = bare.unwrap_err();
        assert!(bare.contains("(Empty) has no clips") && bare.contains(&format!("{} (Bass)", bass.0)), "{bare}");
        let missing = direct(FilmDirection { from_bar: Some(1.0), parts: Some(vec![999]), ..Default::default() });
        assert!(missing.unwrap_err().contains("999"));
    }

    #[test]
    fn a_plan_lists_its_takes_with_bars_shots_and_parts() {
        let (project, _, lead) = song();
        let score = Score::read(&project);
        let directions = [Direction { from: 32.0, to: 64.0, kind: Some(Kind::Close), rolls: vec![lead], entry: None }];
        let plan = Plan::directed(&score, &directions);
        let reply = plan_json(&score, &plan, 0.0, 64.0);
        assert_eq!((reply["bars"].as_str(), reply["seconds"].as_f64()), (Some("1-16"), Some(32.0)));
        let takes = reply["takes"].as_array().unwrap();
        let chorus = takes.iter().find(|take| take["bars"] == "9-16").unwrap();
        assert_eq!(chorus["shot"], "close");
        assert_eq!(chorus["parts"], json!(["Lead"]));
        assert_eq!((chorus["section"].as_str(), chorus["enter"].as_str()), (Some("Chorus"), Some("cut")));
        assert_eq!(chorus["at_seconds"], 16.0);
        assert_eq!(reply["parts"].as_array().unwrap().len(), 2, "the tracks that can be shown");
        // Of a stretch, only its takes, counted from where the film begins.
        let part = plan_json(&score, &plan, 32.0, 64.0);
        assert_eq!(part["takes"].as_array().unwrap().len(), 1);
        assert_eq!((part["takes"][0]["at_seconds"].as_f64(), part["bars"].as_str()), (Some(0.0), Some("9-16")));
        let four = MeterMap::constant(trill_core::TimeSignature::default());
        assert_eq!((bars_text(0.0, 4.0, &four), bars_text(8.0, 9.0, &four)), ("1".to_string(), "3".to_string()));
        assert!(reply.get("tempo_and_meter").is_none(), "a song that changes neither says nothing of it");
    }

    #[test]
    fn a_plan_counts_bars_and_seconds_through_a_change_of_tempo_and_meter_and_says_where_it_is() {
        // Two bars of 4/4 at 120, two of 7/8 at 140, two of 4/4 at 120.
        let mut project = Project::new("Changing");
        let bass = project.add_track("Bass");
        let clip = project.add_midi_clip(bass, "bass", 0.0, 23.0).unwrap();
        let notes: Vec<Note> = (0..46)
            .map(|n| Note { pitch: 36, start: f64::from(n) * 0.5, length: 0.4, velocity: 100, ..Default::default() })
            .collect();
        project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
        project.set_time_signature_at(8.0, 7, 8).unwrap();
        project.set_tempo_at(8.0, 140.0, false).unwrap();
        project.set_time_signature_at(15.0, 4, 4).unwrap();
        project.set_tempo_at(15.0, 120.0, false).unwrap();
        let score = Score::read(&project);
        let reply = plan_json(&score, &Plan::direct(&score), 0.0, score.length);
        assert_eq!((reply["bars"].as_str(), reply["seconds"].as_f64()), (Some("1-6"), Some(11.0)));
        assert_eq!(
            reply["tempo_and_meter"],
            json!([
                { "bar": 3, "at_seconds": 4.0, "to": "7/8 \u{00B7} 140 BPM" },
                { "bar": 5, "at_seconds": 7.0, "to": "4/4 \u{00B7} 120 BPM" },
            ])
        );
        // Bars asked for are the song's: bar 3 is where the 7/8 begins, and
        // bars 3 to 4 are its seven beats.
        let direct = |direction: FilmDirection| directions_of(&project, &score, &[direction]).unwrap().remove(0);
        let seven = direct(FilmDirection {
            from_bar: Some(3.0),
            to_bar: Some(4.0),
            shot: Some(FilmShot::Wide),
            ..Default::default()
        });
        assert_eq!((seven.from, seven.to), (8.0, 15.0));
        let last = direct(FilmDirection { from_bar: Some(6.0), shot: Some(FilmShot::Close), ..Default::default() });
        assert_eq!((last.from, last.to), (19.0, 23.0));
        let beyond = directions_of(
            &project,
            &score,
            &[FilmDirection { from_bar: Some(7.0), shot: Some(FilmShot::Wide), ..Default::default() }],
        );
        assert!(beyond.unwrap_err().contains("bars 1 to 6"));
        // Of the 7/8 alone: its two bars, three seconds, the change at its start.
        let part = plan_json(&score, &Plan::direct(&score), 8.0, 15.0);
        assert_eq!((part["bars"].as_str(), part["seconds"].as_f64()), (Some("3-4"), Some(3.0)));
        assert_eq!(part["tempo_and_meter"][0]["at_seconds"], 0.0);
    }
}
