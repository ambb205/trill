//! The making of a song, staged: its journal played again as a film.
//!
//! The journal's replay ([`trill_core::journal::replay`]) says, for any
//! moment of its own clock (the waiting cut out), what the song was, which
//! step was under way and where it landed. This module puts that on the
//! stage:
//!
//! * **The camera goes where each step landed.** A step that wrote one
//!   part's notes is watched close, on that part's piano roll; one that
//!   touched several parts, from further off, on their lanes; and between
//!   two places the camera travels. Steps that follow one another in the
//!   same stretch of the same part are one shot, so the camera rests while
//!   a part is being written. This is decided once, from the journal alone
//!   ([`shots`]), as the director decides a song's.
//! * **Notes arrive lit**, in the order they sound, as the replay hands
//!   them over; nothing else lights, since nothing plays.
//! * **Words have their place.** The step under way is said in a line at
//!   the foot of the picture. What was asked, what was answered and the
//!   plan are cards, as in a silent film: the stage steps back and the
//!   words have the frame for as long as the replay gives them.
//! * **Its time is the story's.** A film second is a second of the
//!   replay's clock at speed 1, or less of one at a higher speed.
//!
//! **What it sounds like.** The song as it was at a moment cannot be heard
//! without rendering it there, and a session has hundreds of moments. So a
//! making-of is a silent film with music: the finished song plays under
//! it, and the film is timed to end as the song does, on its last bars,
//! with the whole arrangement in the frame. The music sets the picture's
//! mood as it does on the stage (and its kicks still move the picture);
//! what lights is what is written.

use crate::camera::{self, Camera, Lane, Roll};
use crate::export::{Exported, notes_of, score_of, sound_file, sound_of, write_sound};
use crate::film::{Audio, Filmer, Phase, Progress, Shoot, Subject};
use crate::picture::{Look, Picture};
use crate::scene::{Caption, Card, Done, Moment, Story, Written};
use crate::score::Score;
use crate::sound::{Heard, Sound};
use crate::stage::Stage;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use trill_core::journal::replay::{Cursor, Replay};
use trill_core::journal::{Author, Journal, PlanState, What};
use trill_core::{Beats, Project, TrackId, TrackKind};
use trill_engine::render::{RenderJob, RenderOptions};

/// Seconds of film before the story begins, on its title, and after it
/// ends, on the finished song.
const LEAD_IN: f64 = 1.6;
const HOLD: f64 = 4.0;
/// Seconds of the story a move of the camera takes, and seconds of film
/// the move out to the finished song does.
const MOVE: f64 = 0.5;
const MOVE_OUT: f64 = 1.4;
/// The share of the stage a part held close has.
const CLOSE_SHARE: f32 = 0.72;
/// A shot holds at least this many bars: close, and from further off.
const FEWEST_BARS: (f64, f64) = (2.0, 8.0);
/// Steps are one shot while what they cover together is no longer than
/// this many bars: close, and from further off.
const MOST_BARS: (f64, f64) = (16.0, 64.0);
/// A part needs this many notes for a step on it to be watched close.
const CLOSE_NOTES: usize = 4;
/// Seconds of film a card takes to come and to go, and a caption's words
/// to stop glowing.
/// How much of a cut's flash a cut of the story has, and the seconds of
/// film it takes to fall away.
const CUT_FLASH: f32 = 0.45;
const CUT_FALL: f64 = 0.1;
const CARD_COMES: f64 = 0.18;
const CARD_GOES: f64 = 0.25;
const CAPTION_FALL: f64 = 0.3;

/// One shot of the story: where the camera is from a moment on.
#[derive(Clone, Debug, PartialEq)]
pub struct StoryShot {
    /// When it begins, in the story's seconds.
    pub from: f64,
    /// The part held close, if one is.
    pub close: Option<TrackId>,
    /// The stretch of the song in the frame, in beats.
    pub span: (Beats, Beats),
    /// For a part held close: the pitches its roll shows, and the drums
    /// if it is a drum part, as they are once the shot's steps are made.
    pub range: (f32, f32),
    pub drums: Vec<(u8, f32)>,
}

/// The stretch of the song a shot shows for what its steps cover: all of
/// it with room to spare, and never less than a few bars.
fn fit(covers: (Beats, Beats), close: bool, bar: Beats) -> (Beats, Beats) {
    let length = covers.1 - covers.0;
    let fewest = if close { FEWEST_BARS.0 } else { FEWEST_BARS.1 } * bar;
    let window = ((length * 1.25).max(fewest) / bar).ceil() * bar;
    let origin = covers.0 - (window - length) / 2.0;
    (origin, origin + window)
}

/// The shots of a story: decided from the journal alone, so the same
/// journal is always filmed the same way.
pub fn shots(replay: &Replay, drums: &[TrackId]) -> Vec<StoryShot> {
    // What each moment asks for: the part to hold close, and the stretch
    // its step covers.
    struct Asks {
        close: Option<TrackId>,
        covers: Option<(Beats, Beats)>,
    }
    let mut cursor = Cursor::new(replay);
    let mut asks = Vec::with_capacity(replay.moments().len());
    let mut scores: Vec<Option<Score>> = Vec::with_capacity(replay.moments().len());
    let mut bar = 4.0;
    for moment in replay.moments() {
        let frame = replay.frame(moment.starts + moment.lasts * 0.999, &mut cursor);
        bar = frame.song.time_signature.beats_per_bar();
        // Only a step asks for a shot: what is said, and a look, is watched
        // from where the camera is.
        let made = replay.entry(moment).step().is_some_and(|step| !step.aside);
        let covers = frame.focus.span.filter(|(from, to)| made && to > from);
        let alone = match frame.focus.tracks.as_slice() {
            [track] => frame.song.track(*track).ok(),
            _ => None,
        };
        let written = |track: &trill_core::Track| track.clips.iter().map(|clip| clip.notes().len()).sum::<usize>();
        // Close on a part only where the step wrote in it: an envelope
        // or a device is watched on the lanes, where it shows.
        let close = alone
            .filter(|track| covers.is_some() && track.kind == TrackKind::Normal && written(track) >= CLOSE_NOTES)
            .filter(|_| moment.arrivals > 0)
            .map(|track| track.id);
        scores.push(close.map(|_| score_of(frame.song, drums)));
        asks.push(Asks { close, covers });
    }

    // Until a step lands somewhere, the first bars of an empty song.
    let mut shots = vec![StoryShot {
        from: 0.0,
        close: None,
        span: fit((0.0, FEWEST_BARS.1 * bar), false, bar),
        range: (48.0, 72.0),
        drums: Vec::new(),
    }];
    let mut covered = (0.0, 0.0);
    for (index, moment) in replay.moments().iter().enumerate() {
        let Some(covers) = asks[index].covers else { continue };
        let close = asks[index].close;
        let last = shots.last().expect("there is always a first shot");
        let within = last.close == close && covered.0 <= covers.0 && covers.1 <= covered.1;
        if !within {
            // A new shot, which takes in the steps that follow in the same
            // place while together they are not too much for one frame.
            let most = if close.is_some() { MOST_BARS.0 } else { MOST_BARS.1 } * bar;
            covered = covers;
            for later in asks[index + 1..].iter().filter(|later| later.covers.is_some()) {
                let Some(next) = later.covers else { continue };
                let both = (covered.0.min(next.0), covered.1.max(next.1));
                if later.close != close || both.1 - both.0 > most {
                    break;
                }
                covered = both;
            }
            let span = fit(covered, close.is_some(), bar);
            shots.push(StoryShot { from: moment.starts, close, span, range: (48.0, 72.0), drums: Vec::new() });
        }
        // What the part's roll shows, as the part is by now.
        let shot = shots.last_mut().expect("just looked at or pushed");
        if let (Some(track), Some(score)) = (shot.close, &scores[index])
            && let Some(part) = score.part(track)
        {
            shot.range = camera::roll_range(part, shot.span.0, shot.span.1);
            shot.drums = camera::drums_played(part, shot.span.0, shot.span.1);
        }
    }
    shots
}

/// The story of a journal, staged: something a film can be made of.
pub struct Making {
    replay: Replay,
    cursor: Cursor,
    shots: Vec<StoryShot>,
    drums: Vec<TrackId>,
    /// Where on the story's length the person spoke, 0..=1.
    turns: Vec<f32>,
    /// For every moment, how many steps had been made by its end.
    steps: Vec<usize>,
    /// How many seconds of the story a second of film is.
    speed: f64,
    /// The finished song, for its name, its last picture and the hits of
    /// the music under the film.
    finished: Score,
    /// The music under the film, and where in it the film begins.
    sound: Sound,
    sound_from: f64,
    /// The song as the replay last had it, read once for each state.
    scored: Option<(u64, Score)>,
    /// What the cards are made of, kept between pictures.
    plan: Vec<(String, Done)>,
}

impl Making {
    /// The story of `journal` at `speed`, with `finished` the song it
    /// came to. `drums` are the tracks that play through a drum
    /// instrument, where the song does not say what their notes are.
    pub fn of(journal: Journal, finished: &Project, drums: &[TrackId], speed: f64) -> Result<Self, String> {
        let replay = Replay::new(journal);
        if replay.moments().is_empty() || replay.length() <= 0.0 {
            return Err("The journal has nothing in it to show: no step was made while it was kept.".to_string());
        }
        let shots = shots(&replay, drums);
        let length = replay.length() as f32;
        let spoke = |moment: &&trill_core::journal::replay::Moment| {
            let entry = replay.entry(moment);
            entry.by == Author::Person && matches!(entry.what, What::Said { .. })
        };
        let turns = replay.moments().iter().filter(spoke).map(|moment| moment.starts as f32 / length).collect();
        let mut made = 0;
        let steps = replay
            .moments()
            .iter()
            .map(|moment| {
                made += usize::from(replay.entry(moment).step().is_some_and(|step| !step.aside));
                made
            })
            .collect();
        Ok(Self {
            cursor: Cursor::new(&replay),
            replay,
            shots,
            drums: drums.to_vec(),
            turns,
            steps,
            speed: speed.max(0.05),
            finished: score_of(finished, drums),
            sound: Sound::default(),
            sound_from: 0.0,
            scored: None,
            plan: Vec::new(),
        })
    }

    /// Puts music under the film: `sound`, heard from `from` seconds of it
    /// as the film begins.
    pub fn with_sound(mut self, sound: Sound, from: f64) -> Self {
        (self.sound, self.sound_from) = (sound, from);
        self
    }

    /// How long the story is at speed 1, in seconds.
    pub fn story_length(&self) -> f64 {
        self.replay.length()
    }

    /// How long the film is, in seconds: its title, the story at the
    /// film's speed, and the finished song held at the end.
    pub fn length(&self) -> f64 {
        LEAD_IN + self.replay.length() / self.speed + HOLD
    }

    /// The speed at which the film of a story `story` seconds long lasts
    /// `seconds`, title and ending counted.
    pub fn speed_to_last(story: f64, seconds: f64) -> f64 {
        let room = seconds - LEAD_IN - HOLD;
        if room > 0.0 { story / room } else { 1.0 }
    }

    /// When in the film a moment of the story is, in seconds: after the
    /// title, at the film's speed.
    pub fn film_time(&self, story: f64) -> f64 {
        LEAD_IN + story / self.speed
    }

    /// The shots of the story, in order.
    pub fn shots(&self) -> &[StoryShot] {
        &self.shots
    }

    /// How many steps the story has, and what is wrong with the journal if
    /// the story had to end before its last entry.
    pub fn steps(&self) -> usize {
        self.steps.last().copied().unwrap_or(0)
    }

    pub fn broken(&self) -> Option<&str> {
        self.replay.broken()
    }

    /// The camera `through` (0..=1, eased by the caller) of the way from
    /// the shot numbered `index - 1` to the one numbered `index`, and
    /// `closing` of the way out to the whole of the song.
    fn camera(shots: &[StoryShot], score: &Score, index: usize, through: f32, closing: f32) -> Camera {
        let shot = &shots[index];
        let before = if index > 0 { &shots[index - 1] } else { shot };
        let middle = |span: (Beats, Beats)| ((span.0 + span.1) / 2.0, span.1 - span.0);
        let ((from_at, from_wide), (to_at, to_wide)) = (middle(before.span), middle(shot.span));
        let blend = f64::from(through);
        let mut at = from_at + (to_at - from_at) * blend;
        // The zoom travels as a ratio, so a move keeps one pace.
        let mut wide = (from_wide.ln() + (to_wide.ln() - from_wide.ln()) * blend).exp();

        let has_notes = |track: TrackId| score.part(track).is_some_and(|part| !part.notes.is_empty());
        let mut rolls = Vec::new();
        let roll = |shot: &StoryShot, weight: f32| {
            let track = shot.close.filter(|track| has_notes(*track))?;
            Some(Roll { track, weight, low: shot.range.0, high: shot.range.1, drums: shot.drums.clone() })
        };
        let same = before.close == shot.close;
        rolls.extend(roll(shot, if same { 1.0 } else { through }));
        if !same && through < 1.0 {
            rolls.extend(roll(before, 1.0 - through));
        }
        let share = |shot: &StoryShot| if shot.close.is_some_and(has_notes) { CLOSE_SHARE } else { 0.0 };
        let mut roll_share = share(before) + (share(shot) - share(before)) * through;

        // At the end the camera moves out to the whole of the finished song.
        if closing > 0.0 {
            let margin = score.length * 0.03;
            let whole = ((score.length) / 2.0, score.length + 2.0 * margin);
            at += (whole.0 - at) * f64::from(closing);
            wide = (wide.ln() + (whole.1.ln() - wide.ln()) * f64::from(closing)).exp();
            roll_share *= 1.0 - closing;
            rolls.iter_mut().for_each(|roll| roll.weight *= 1.0 - closing);
        }
        if rolls.iter().all(|roll| roll.weight <= 0.0) {
            (rolls, roll_share) = (Vec::new(), 0.0);
        }
        Camera {
            beat: at,
            origin: at - wide / 2.0,
            window: wide,
            roll_share,
            rolls,
            lanes: score.parts.iter().map(|part| Lane { track: part.track, open: 1.0 }).collect(),
            take: index,
            overview: closing,
            travel: 1.0 - (2.0 * through - 1.0).abs(),
            since_cut: None,
        }
    }
}

/// "12:41 in", "1:07:41 in": how long into the work something was.
fn into_the_work(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    match seconds / 3600 {
        0 => format!("{}:{:02} in", seconds / 60, seconds % 60),
        hours => format!("{hours}:{:02}:{:02} in", seconds / 60 % 60, seconds % 60),
    }
}

/// "14 min", "2 h 5 min", "40 s": how long the work took.
fn how_long(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    match (seconds / 3600, seconds / 60 % 60) {
        (0, 0) => format!("{seconds} s"),
        (0, minutes) => format!("{minutes} min"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// Who made an entry, as a caption says it.
fn who(author: &Author) -> &str {
    match author {
        Author::Person => "You",
        Author::Assistant => "The assistant",
        Author::Client(name) if name.is_empty() => "A client",
        Author::Client(name) => name,
    }
}

impl Subject for Making {
    fn picture(&mut self, stage: &mut Stage, seconds: f64, shoot: &Shoot, seed: u32) -> Picture {
        let story_length = self.replay.length();
        let story = (seconds - LEAD_IN) * self.speed;
        let ended = seconds - (LEAD_IN + story_length / self.speed);
        let frame = self.replay.frame(story.clamp(0.0, story_length), &mut self.cursor);
        if self.scored.as_ref().is_none_or(|(version, _)| *version != frame.version) {
            self.scored =
                Some((frame.version, Score::of_every_track(frame.song, |track| self.drums.contains(&track.id))));
        }
        let score = &self.scored.as_ref().expect("read just above").1;

        // What was just written, lit by how new it is.
        let fresh = self.replay.pace().glow;
        let written: Vec<Written> = frame
            .arrived
            .iter()
            .map(|arrived| Written {
                track: arrived.track,
                clip: arrived.clip,
                pitch: arrived.note.map(|note| note.pitch),
                beat: arrived.beat,
                glow: (1.0 - (arrived.age / fresh).clamp(0.0, 1.0) as f32).powf(0.7),
            })
            .collect();

        // The words: the step under way, or a card.
        let moment = frame.moment.map(|index| (index, self.replay.moments()[index]));
        let entry = moment.map(|(_, moment)| self.replay.entry(&moment));
        let since = moment.map_or(0.0, |(_, moment)| (story - moment.starts).max(0.0) / self.speed);
        let left = moment.map_or(0.0, |(_, moment)| (moment.ends() - story).max(0.0) / self.speed);
        let comes_and_goes = ((since / CARD_COMES).min(1.0) * (left / CARD_GOES).min(1.0)) as f32;
        let began = self.replay.journal().began();
        let total = self.steps.last().copied().unwrap_or(0);
        let made = moment.map_or(0, |(index, _)| self.steps[index]);
        let count = match made {
            0 => format!("{total} steps"),
            made => format!("Step {made} of {total}"),
        };
        let clock = entry.map_or_else(String::new, |entry| into_the_work(entry.at.saturating_sub(began)));
        let over = format!("{total} steps \u{00B7} {} of work", {
            let last = self.replay.journal().last().map_or(began, |entry| entry.at);
            how_long(last.saturating_sub(began))
        });
        if let Some(What::Plan { items }) = entry.map(|entry| &entry.what) {
            self.plan = items
                .iter()
                .map(|item| {
                    let state = match item.state {
                        PlanState::Pending => Done::Pending,
                        PlanState::Doing => Done::Doing,
                        PlanState::Done => Done::Done,
                    };
                    (item.text.clone(), state)
                })
                .collect();
        }
        let name = self.finished.name.as_str();
        let mut step = None;
        let mut card = None;
        if story < 0.0 {
            // The title, which goes as the story begins.
            let goes = ((LEAD_IN - seconds) / 0.4).clamp(0.0, 1.0) as f32;
            card = Some((Card::Title { over: "The making of", title: name }, goes));
        } else if ended >= 0.0 {
            card = Some((Card::Title { over: &over, title: name }, (ended / 0.8).clamp(0.0, 1.0) as f32));
        } else if let Some(entry) = entry {
            let glow = (-since / CAPTION_FALL).exp() as f32;
            match &entry.what {
                What::Said { text } => {
                    let person = entry.by == Author::Person;
                    let label = if person { "Asked" } else { who(&entry.by) };
                    card = Some((Card::Said { who: label, text, person }, comes_and_goes));
                }
                What::Plan { .. } => card = Some((Card::Plan { items: &self.plan }, comes_and_goes)),
                What::Step(made) => {
                    step = Some(Caption { who: who(&entry.by), what: &made.label, heard: made.heard.as_deref(), glow });
                }
                What::Look(look) => {
                    step = Some(Caption { who: who(&entry.by), what: &look.label, heard: look.heard.as_deref(), glow });
                }
                What::Mark { label } => step = Some(Caption { who: who(&entry.by), what: label, heard: None, glow }),
            }
        }

        // The camera: in the shot the story is in, on its way from the
        // one before; and out to the whole song once the story is told.
        // Between two stretches of one part the camera travels; to another
        // part, or between a part and the lanes, it cuts, as to a new
        // subject.
        let index = self.shots.partition_point(|shot| shot.from <= story).saturating_sub(1);
        let since_shot = (story - self.shots[index].from).max(0.0);
        let cuts = index > 0 && self.shots[index - 1].close != self.shots[index].close;
        let through = match index {
            0 => 1.0,
            _ if cuts => 1.0,
            _ => camera::ease((since_shot / MOVE).clamp(0.0, 1.0) as f32),
        };
        let cut = if cuts { CUT_FLASH * (-(since_shot / self.speed) / CUT_FALL).exp() as f32 } else { 0.0 };
        let closing = camera::ease((ended / MOVE_OUT).clamp(0.0, 1.0) as f32);
        let camera = Self::camera(&self.shots, score, index, through, closing);

        // The music under the film sets the mood, and its kicks move the
        // picture; what is written sets what lights.
        let music = self.sound_from + seconds;
        let heard = match self.sound.duration() > 0.0 {
            true => Heard { tracks: Vec::new(), ..self.sound.heard(music) },
            false => Heard { level: 0.6, punch: 0.3, tracks: Vec::new() },
        };
        let mut look = Look::struck(&self.finished, self.finished.beat_at(music), &heard, cut, seed);
        // Words are being read: the picture is moved less than a song's.
        (look.punch, look.tear) = (look.punch * 0.5, 0.0);
        let told = Story {
            written: &written,
            step,
            card,
            progress: (story / story_length).clamp(0.0, 1.0) as f32,
            turns: &self.turns,
            count: &count,
            clock: &clock,
            about: if closing > 0.0 { None } else { frame.focus.span },
        };
        let moment = Moment { score, camera: &camera, heard: &heard, shot: "Making of", story: Some(&told) };
        stage.render(&moment, shoot.format, shoot.oversample, look)
    }
}

/// What a film of a making-of is asked to be.
#[derive(Clone, Debug, PartialEq)]
pub struct MakingRequest {
    pub path: PathBuf,
    pub shoot: Shoot,
    /// How many seconds of the story a second of film is. `None` for 1,
    /// or as much more as it takes for the film to be no longer than the
    /// song that plays under it.
    pub speed: Option<f64>,
    /// Whether the finished song plays under it.
    pub music: bool,
    pub sample_rate: u32,
    pub patience: Duration,
}

impl MakingRequest {
    pub fn new(path: impl Into<PathBuf>, shoot: Shoot) -> Self {
        Self {
            path: path.into(),
            shoot,
            speed: None,
            music: true,
            sample_rate: 48_000,
            patience: Duration::from_secs(10),
        }
    }
}

/// A film of the making of a song.
pub struct MakingFilm {
    /// The story.
    pub journal: Journal,
    /// The finished song: what the film ends on, and what plays under it.
    pub render: RenderJob,
    pub drums: Vec<TrackId>,
    pub request: MakingRequest,
}

impl MakingFilm {
    /// Makes the film. The error is a sentence.
    pub fn run(self, progress: &Progress) -> Result<Exported, String> {
        let outcome = self.make(progress);
        progress.enter(Phase::Done);
        outcome
    }

    /// The story as it will be filmed, at the speed the film will have.
    /// Nothing is rendered for this, so a plan can be read from it.
    ///
    /// A film is no longer than its music unless a speed was asked for:
    /// the story is told as much faster as it takes to end with the song.
    pub fn making(&self) -> Result<Making, String> {
        let request = &self.request;
        let speed = request.speed.unwrap_or(1.0);
        let mut making = Making::of(self.journal.clone(), &self.render.project, &self.drums, speed)?;
        let song = making.finished.duration();
        if request.speed.is_none() && request.music && making.length() > song && song > LEAD_IN + HOLD + 4.0 {
            making.speed = Making::speed_to_last(making.story_length(), song).max(1.0);
        }
        Ok(making)
    }

    fn make(self, progress: &Progress) -> Result<Exported, String> {
        let started = Instant::now();
        let request = &self.request;
        let mut filmer = Filmer::new()?;
        let finished = &self.render.project;
        let mut making = self.making()?;
        let mut notes = Vec::new();
        if let Some(broken) = making.broken() {
            notes.push(format!("The journal could not be followed to its end ({broken}); the film stops there."));
        }

        // The music: the finished song, rendered as an export renders it.
        let wav = sound_file(&request.path);
        let mut audio = None;
        let (mut loudness, mut peak) = (None, None);
        if request.music && finished.length_beats() > 0.0 {
            progress.enter(Phase::Sound);
            let options = RenderOptions {
                sample_rate: request.sample_rate,
                patience: request.patience,
                max_tail_seconds: 4.0,
                ..RenderOptions::default()
            };
            let rendered = crate::export::sound(&self.render, options, progress)?;
            let song = rendered.left.len() as f64 / f64::from(rendered.sample_rate);
            // The film ends as the song does: on its last bars.
            let length = making.length();
            let (from, delay) = ((song - length).max(0.0), (length - song).max(0.0));
            notes.extend(notes_of(&rendered));
            write_sound(&wav, &rendered)?;
            (loudness, peak) = (Some(rendered.measured.stats.loudness_lufs), Some(rendered.measured.stats.peak_db));
            making = making.with_sound(sound_of(&rendered), from - delay);
            audio = Some(Audio { path: wav.clone(), from, delay, fade_in: if from > 0.0 { 1.5 } else { 0.0 } });
        }

        let seconds = making.length();
        let filmed = filmer.film_of(&mut making, 0.0, seconds, &request.shoot, audio.as_ref(), &request.path, progress);
        crate::film::remove(&wav);
        let filmed = filmed?;
        notes.extend(filmed.note);
        Ok(Exported {
            path: filmed.path,
            frames: filmed.frames,
            seconds,
            took: started.elapsed().as_secs_f64(),
            notes,
            loudness_lufs: loudness,
            peak_db: peak,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::journal::{Outcome, PlanItem, Who};
    use trill_core::{Note, Project};

    /// Writes a song step by step into a journal: two tracks, a bass line
    /// in two halves, a lead far along the song, and a conversation.
    fn story() -> (Journal, Project, TrackId, TrackId) {
        let mut song = Project::new("Made");
        let mut journal = Journal::begin(&song, 0);
        let mut at = 0;
        let mut step = |journal: &mut Journal, song: &mut Project, label: &str, change: &dyn Fn(&mut Project)| {
            at += 20_000;
            let before = song.clone();
            change(song);
            let call = journal.begin_call(Author::Assistant, "write", label);
            journal.record(at, Who::Call(call), &before, song, None);
            journal.end_call(at, call, Outcome::default());
        };
        journal.say(1_000, Author::Person, "Write me a bass line and a lead.");
        let items = ["Bass", "Lead"].map(|text| PlanItem { text: text.to_string(), state: PlanState::Pending });
        journal.plan(2_000, items.to_vec());
        step(&mut journal, &mut song, "Added the tracks", &|song| {
            song.add_track("Bass");
            song.add_track("Lead");
        });
        let (bass, lead) = (song.tracks[0].id, song.tracks[1].id);
        let line = |from: u32, pitch: u8| -> Vec<Note> {
            (0..16)
                .map(|n| Note {
                    pitch: pitch + (n % 5) as u8,
                    start: f64::from(from + n) * 0.5,
                    length: 0.4,
                    velocity: 100,
                })
                .collect()
        };
        step(&mut journal, &mut song, "Wrote the first bars of the bass", &|song| {
            let clip = song.add_midi_clip(bass, "bass", 0.0, 16.0).unwrap();
            song.clip_mut(clip).unwrap().add_notes(&line(0, 36)).unwrap();
        });
        step(&mut journal, &mut song, "Wrote the rest of the bass", &|song| {
            let clip = song.tracks[0].clips[0].id;
            song.clip_mut(clip).unwrap().add_notes(&line(16, 36)).unwrap();
        });
        step(&mut journal, &mut song, "Wrote the lead", &|song| {
            let clip = song.add_midi_clip(lead, "lead", 64.0, 8.0).unwrap();
            song.clip_mut(clip).unwrap().add_notes(&line(0, 72)).unwrap();
        });
        journal.say(90_000, Author::Assistant, "Done: a bass line and a lead over it.");
        (journal, song, bass, lead)
    }

    #[test]
    fn the_camera_goes_where_each_step_landed_and_rests_while_a_part_is_written() {
        let (journal, _, bass, lead) = story();
        let replay = Replay::new(journal);
        let shots = shots(&replay, &[]);
        let seen: Vec<(Option<TrackId>, (Beats, Beats))> = shots.iter().map(|shot| (shot.close, shot.span)).collect();
        // The empty song; then the bass, close, in one shot for both of
        // the steps that wrote it; then the lead, where it is.
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert_eq!(seen[0].0, None);
        assert_eq!(seen[1].0, Some(bass));
        assert!(seen[1].1.0 <= 0.0 && seen[1].1.1 >= 16.0, "all of the clip: {:?}", seen[1].1);
        assert_eq!(seen[2].0, Some(lead));
        assert!(seen[2].1.0 <= 64.0 && seen[2].1.1 >= 72.0 && seen[2].1.0 > 40.0, "{:?}", seen[2].1);
        // A shot begins with the step that needs it, and they are in order.
        let step_times: Vec<f64> = replay
            .moments()
            .iter()
            .filter(|moment| replay.entry(moment).step().is_some())
            .map(|moment| moment.starts)
            .collect();
        assert_eq!((shots[1].from, shots[2].from), (step_times[1], step_times[3]));
        // The roll of the bass shows the notes it has once both steps are made.
        assert!(shots[1].range.0 < 36.0 && shots[1].range.1 > 40.0, "{:?}", shots[1].range);
        // The same journal is always shot the same way.
        assert_eq!(shots, super::shots(&replay, &[]));
    }

    #[test]
    fn a_film_is_its_title_the_story_at_its_speed_and_the_finished_song_held() {
        let (journal, song, ..) = story();
        let making = Making::of(journal.clone(), &song, &[], 1.0).unwrap();
        let story = making.story_length();
        assert!(story > 3.0, "four steps, two things said and a plan: {story}");
        assert_eq!(making.length(), LEAD_IN + story + HOLD);
        assert_eq!(making.steps(), 4);
        let faster = Making::of(journal, &song, &[], 2.0).unwrap();
        assert_eq!(faster.length(), LEAD_IN + story / 2.0 + HOLD);
        // The speed at which it lasts as long as a song of a minute.
        let speed = Making::speed_to_last(story, 60.0);
        assert!((LEAD_IN + story / speed + HOLD - 60.0).abs() < 1e-9);
        // A journal in which nothing was made is not a film.
        let empty = Journal::begin(&Project::new("Empty"), 0);
        let refused = Making::of(empty, &Project::new("Empty"), &[], 1.0).err().unwrap();
        assert!(refused.contains("nothing in it to show"), "{refused}");
    }

    #[test]
    fn times_are_said_as_a_person_would() {
        assert_eq!(into_the_work(0), "0:00 in");
        assert_eq!(into_the_work(761_000), "12:41 in");
        assert_eq!(into_the_work(4_061_000), "1:07:41 in");
        assert_eq!(
            (how_long(40_000), how_long(840_000), how_long(7_500_000)),
            ("40 s".into(), "14 min".into(), "2 h 5 min".into())
        );
    }

    #[test]
    fn a_shot_holds_what_its_steps_cover_with_room_to_spare() {
        // A two-bar phrase, close: three bars, the phrase in the middle.
        assert_eq!(fit((8.0, 16.0), true, 4.0), (6.0, 18.0));
        // A single beat is still shown in two bars, and from further off in eight.
        assert_eq!(fit((8.0, 9.0), true, 4.0), (4.5, 12.5));
        assert_eq!(fit((8.0, 9.0), false, 4.0), (-7.5, 24.5));
    }
}
