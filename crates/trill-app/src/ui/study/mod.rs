//! Studies in the window: recordings taken apart, and what is taken from
//! them into the song.
//!
//! Three places show them. The **workspace** ([`view`]) is one study laid
//! out: its stems as lanes under the recording's own bars, its parts,
//! chords and words along them, and beside them what was found (loops,
//! single hits, lines, notes). A stretch is selected across the lanes,
//! played round as it would sound in the song, and dragged or sent there.
//! The **Studies place** of the browser ([`place`]) lists every study,
//! the ones being made with the stage they are at. The **dialog** asks
//! for a recording to study: a file or a link.
//!
//! The window does nothing of its own with a study. What it shows was
//! read by a helper thread (a study is files); what it takes into the
//! song is taken by `trill_runtime::taking`, which the tools use too; and
//! what is heard before anything is taken is made by the same plan.

pub mod place;
pub mod view;
pub mod window;

use super::sounds::DropTarget;
use super::state::Cx;
use eframe::egui;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use trill_core::SongKey;
use trill_listen::{Correction, Heard, MidiPart};
use trill_runtime::Runtime;
use trill_runtime::study_files::UsedBy;
use trill_runtime::taking::{self, HitTo, Lane, Pitch, TakeNotes, TakeStretch};
use trill_study::store::Outlines;
use trill_study::{Lyrics, Report, Source, State, StudyOptions};

/// How often the list is read again while a study is being made, and
/// while none is, in seconds.
const READ_EVERY_BUSY: f64 = 0.25;
const READ_EVERY: f64 = 2.0;

/// A study that is open in the workspace: everything it draws, read once.
pub struct Open {
    pub id: String,
    pub title: String,
    pub seconds: f64,
    /// The names of its stems, in the order of its lanes, and whether each
    /// one's file is there.
    pub stems: Vec<(String, PathBuf, bool)>,
    pub heard: Arc<Heard>,
    pub lyrics: Option<Lyrics>,
    pub outlines: Option<Outlines>,
    /// The start of every bar, closed with the end of the last.
    pub bars: Vec<f64>,
    pub key: Option<SongKey>,
    pub corrections: Vec<Correction>,
    /// Where the recording came from, in a few words.
    pub of: String,
}

impl Open {
    /// The nearest bar line, or beat, to a moment of the recording.
    pub fn snapped(&self, seconds: f64, snap: Snap) -> f64 {
        let nearest = |lines: &[f64]| {
            let after = lines.partition_point(|line| *line < seconds);
            let candidates = [after.checked_sub(1).and_then(|index| lines.get(index)), lines.get(after)];
            candidates.into_iter().flatten().copied().min_by(|a, b| (a - seconds).abs().total_cmp(&(b - seconds).abs()))
        };
        let found = match snap {
            Snap::Bar => nearest(&self.bars),
            Snap::Beat => nearest(&self.heard.grid.beats),
            Snap::Off => None,
        };
        found.unwrap_or(seconds).clamp(0.0, self.seconds)
    }

    /// The bar a moment lies in, from 1, and `None` before the first line.
    pub fn bar_at(&self, seconds: f64) -> Option<usize> {
        let after = self.bars.partition_point(|line| *line <= seconds + 1e-6);
        (after > 0 && after < self.bars.len().max(1)).then_some(after)
    }
}

/// What the ends of a selection are moved onto.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Snap {
    #[default]
    Bar,
    Beat,
    Off,
}

/// What is listed beside the lanes.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Side {
    #[default]
    Loops,
    Hits,
    Words,
    Notes,
}

/// A stretch selected across lanes.
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub from: f64,
    pub to: f64,
    /// The lanes it covers, first and last, as numbers into the stems.
    pub lanes: (usize, usize),
}

/// How the workspace is looked at: nothing of the study itself.
#[derive(Clone, Debug, Default)]
pub struct View {
    /// The moment at the left edge, and points to a second; 0 fits the
    /// whole recording.
    pub from: f64,
    pub per_second: f64,
    pub selection: Option<Selection>,
    pub muted: BTreeSet<String>,
    pub solo: BTreeSet<String>,
    pub snap: Snap,
    pub side: Side,
    /// Heard, and taken, at the recording's own speed and pitch.
    pub as_recorded: bool,
    /// Moved to the key the song says it is in.
    pub to_song_key: bool,
    /// The word being put right: its line, its place in it, and the text.
    pub word: Option<(usize, usize, String)>,
    /// Where a selection that is being drawn began, in seconds, and the lane.
    pub anchor: Option<(f64, usize)>,
    /// How far the list beside the lanes is scrolled.
    pub side_scroll: f32,
}

/// What of a study is in the air, on its way to the song.
#[derive(Clone, Debug, PartialEq)]
pub enum StudyDrag {
    /// A stretch of stems, and how long it will be in the song, in beats.
    Stretch(TakeStretch, f64),
    Hit {
        study: String,
        id: String,
    },
    Notes {
        study: String,
        part: MidiPart,
        from: f64,
        to: f64,
        beats: f64,
    },
}

impl StudyDrag {
    /// Whether what is dragged can be let go on `target`, or why not.
    pub fn takes(&self, target: &DropTarget) -> Result<(), &'static str> {
        match (self, target) {
            (_, DropTarget::Lane { .. } | DropTarget::NewTrack { .. }) => Ok(()),
            (StudyDrag::Hit { .. }, DropTarget::Pad { .. } | DropTarget::Rack { .. } | DropTarget::Sampler { .. }) => {
                Ok(())
            }
            (StudyDrag::Stretch(..), _) => {
                Err("A stretch of a study goes onto a track, or below the tracks for a new one")
            }
            (StudyDrag::Notes { .. }, _) => Err("Notes go onto a track, or below the tracks for a new one"),
            (StudyDrag::Hit { .. }, _) => Err("A hit goes onto a track, a pad or a sampler"),
        }
    }

    /// How long it will be on a lane, in beats, where that is known.
    pub fn beats(&self) -> Option<f64> {
        match self {
            StudyDrag::Stretch(_, beats) | StudyDrag::Notes { beats, .. } => Some(*beats),
            StudyDrag::Hit { .. } => None,
        }
    }
}

/// A question that stands before a study goes, or its stems.
pub struct Question {
    pub id: String,
    pub title: String,
    /// Only the stems go.
    pub stems: bool,
    pub used: Vec<UsedBy>,
}

/// The dialog that asks for a recording to study.
#[derive(Default)]
pub struct Dialog {
    pub open: bool,
    /// A file's path or a link, as typed.
    pub of: String,
    pub words: bool,
    pub best_stems: bool,
    /// Whether the box that picks a file is up.
    pub picking: bool,
    /// What the last attempt said, where it went wrong.
    pub refused: Option<String>,
}

/// What a helper thread reports back.
enum Done {
    /// Every study there is, or why there are none to show.
    Listed(Result<Vec<Report>, String>),
    Opened(Result<Box<Open>, String>),
    /// Something that was asked for is done; what there is to say of it.
    Said(Result<String, String>),
    /// A study was asked for: its id, or why not.
    Started(Result<String, String>),
    /// What plays a study's files, for the question before it goes.
    Asked(Result<Question, String>),
    /// A stretch is ready to be heard: the key it was made for.
    Sounding(u64, Result<taking::Sounding, String>),
    /// The file that was picked in the dialog's box, if one was.
    Picked(Option<PathBuf>),
    /// The words of the open study, after one was put right.
    Words(String, Result<Lyrics, String>),
    /// Something was taken into the song: the clip to show, if it made one.
    Taken(Result<Option<trill_core::ClipId>, String>),
}

/// Studies, as the window holds them.
pub struct Studies {
    list: Vec<Report>,
    read: bool,
    reading: bool,
    read_at: f64,
    unavailable: Option<String>,
    open: Option<Arc<Open>>,
    /// The study that is being read to be opened.
    opening: Option<String>,
    pub view: View,
    pub dialog: Dialog,
    pub question: Option<Question>,
    /// The study whose own lines the browser shows under its list.
    pub chosen: Option<String>,
    /// What is being played round: the number of the request it answers.
    sounding: Option<u64>,
    /// The request for a sound that is being made.
    preparing: Option<u64>,
    asked: u64,
    working: usize,
    /// What the last thing that was asked for said, and whether it went wrong.
    last: Option<(String, bool)>,
    /// Whether the workspace is shown in a window of its own.
    pub apart: bool,
    /// How the study that is being opened is to be shown.
    pub shown: Option<Shown>,
    sender: Sender<Done>,
    receiver: Receiver<Done>,
}

impl Default for Studies {
    fn default() -> Self {
        let (sender, receiver) = channel();
        Self {
            list: Vec::new(),
            read: false,
            reading: false,
            read_at: f64::NEG_INFINITY,
            unavailable: None,
            open: None,
            opening: None,
            view: View::default(),
            dialog: Dialog { words: true, ..Dialog::default() },
            question: None,
            chosen: None,
            sounding: None,
            preparing: None,
            asked: 0,
            working: 0,
            last: None,
            apart: false,
            shown: None,
            sender,
            receiver,
        }
    }
}

impl Studies {
    /// Every study there is, latest first.
    pub fn list(&self) -> &[Report] {
        &self.list
    }

    /// Why there are none to show, where nothing is kept.
    pub fn unavailable(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// The study that is open in the workspace.
    pub fn open(&self) -> Option<&Arc<Open>> {
        self.open.as_ref()
    }

    /// The study that is being read to be opened.
    pub fn opening(&self) -> Option<&str> {
        self.opening.as_deref()
    }

    /// Whether a selection is being played round, or made ready to be.
    pub fn is_sounding(&self) -> bool {
        self.sounding.is_some() || self.preparing.is_some()
    }

    pub fn is_preparing(&self) -> bool {
        self.preparing.is_some()
    }

    /// What the last thing that was asked for said.
    pub fn last(&self) -> Option<(&str, bool)> {
        self.last.as_ref().map(|(words, wrong)| (words.as_str(), *wrong))
    }

    /// True while something the window shows of studies is on its way: a
    /// picture waits for it.
    pub fn is_waiting(&self) -> bool {
        self.reading || self.opening.is_some() || self.working > 0 || self.preparing.is_some()
    }

    /// Whether a study is being made, here or by another program.
    pub fn making(&self) -> bool {
        self.list.iter().any(|report| matches!(report.study.state, State::Running | State::Waiting))
    }

    fn spawn(
        &self,
        ctx: &egui::Context,
        runtime: &Arc<Runtime>,
        job: impl FnOnce(&Runtime, &dyn Fn(Done)) + Send + 'static,
    ) {
        let (sender, ctx, runtime) = (self.sender.clone(), ctx.clone(), runtime.clone());
        let fallback = self.sender.clone();
        let spawned = std::thread::Builder::new().name("trill-studies-view".into()).spawn(move || {
            job(&runtime, &|done| {
                // The receiver is gone only when the window has closed.
                let _ = sender.send(done);
                ctx.request_repaint();
            });
        });
        if let Err(error) = spawned {
            let _ = fallback.send(Done::Said(Err(format!("a helper thread could not be started: {error}"))));
        }
    }
}

/// Reads one study as the workspace draws it.
fn read(runtime: &Runtime, id: &str) -> Result<Box<Open>, String> {
    let studied = runtime.study(id)?;
    let study = &studied.report.study;
    let Some(heard) = studied.heard.clone().filter(|_| study.state == State::Done) else {
        return Err(match study.state {
            State::Running | State::Waiting => format!("{} is still being studied", study.title),
            State::Failed => format!(
                "{} could not be studied: {}",
                study.title,
                study.error.clone().unwrap_or_else(|| "it did not say why".to_owned())
            ),
            _ => format!("{} was stopped before it was done: take it up again in the Studies place", study.title),
        });
    };
    let stems = study
        .stems
        .iter()
        .map(|name| {
            let file = studied.folder.stem(name);
            (name.clone(), file.clone(), file.is_file())
        })
        .collect();
    let of = match &study.source {
        Source::File { path } => path.display().to_string(),
        Source::Link { url, by } => match by {
            Some(by) => format!("{url} ({by})"),
            None => url.clone(),
        },
    };
    Ok(Box::new(Open {
        id: study.id.clone(),
        title: study.title.clone(),
        seconds: heard.seconds,
        stems,
        bars: heard.grid.bar_times(heard.seconds),
        key: taking::key_of(&heard),
        lyrics: studied.lyrics.clone(),
        outlines: studied.outlines(),
        corrections: study.corrections.clone(),
        of,
        heard,
    }))
}

/// Reads the list again where it is due, and takes in what the helpers
/// have reported. Called once a frame while anything of studies is shown.
pub fn poll(ctx: &egui::Context, cx: &mut Cx, shown: bool) {
    while let Ok(done) = cx.studies.receiver.try_recv() {
        took_in(ctx, cx, done);
    }
    if !shown {
        return;
    }
    let now = ctx.input(|input| input.time);
    let every = if cx.studies.making() { READ_EVERY_BUSY } else { READ_EVERY };
    if !cx.studies.reading && now - cx.studies.read_at >= every {
        (cx.studies.reading, cx.studies.read_at) = (true, now);
        cx.studies.spawn(ctx, &cx.sounds.runtime, |runtime, report| {
            report(Done::Listed(runtime.studies().map(|desk| desk.list())));
        });
    }
    if cx.studies.making() {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(every));
    }
}

fn took_in(ctx: &egui::Context, cx: &mut Cx, done: Done) {
    let studies = &mut *cx.studies;
    match done {
        Done::Listed(listed) => {
            (studies.reading, studies.read) = (false, true);
            match listed {
                Ok(list) => {
                    // A study that came to be done while it was looked at opens by itself.
                    let finished = |id: &str| {
                        let was = studies.list.iter().find(|report| report.study.id == id);
                        let is = list.iter().find(|report| report.study.id == id);
                        was.is_some_and(|was| was.study.state != State::Done)
                            && is.is_some_and(|is| is.study.state == State::Done)
                    };
                    let opened = list.iter().map(|report| report.study.id.clone()).find(|id| finished(id));
                    (studies.list, studies.unavailable) = (list, None);
                    if let Some(id) = opened.filter(|_| studies.open.is_none() && studies.opening.is_none()) {
                        open(ctx, cx, &id);
                    }
                }
                Err(why) => (studies.list, studies.unavailable) = (Vec::new(), Some(why)),
            }
        }
        Done::Opened(opened) => {
            studies.opening = None;
            match opened {
                Ok(opened) => {
                    let same = studies.open.as_ref().is_some_and(|open| open.id == opened.id);
                    if !same {
                        studies.view = View {
                            snap: studies.view.snap,
                            side: studies.view.side,
                            as_recorded: studies.view.as_recorded,
                            to_song_key: studies.view.to_song_key,
                            ..View::default()
                        };
                    }
                    if let Some(shown) = studies.shown.take() {
                        arrange(&mut studies.view, &opened, &shown);
                    }
                    studies.open = Some(Arc::from(opened));
                }
                Err(why) => studies.last = Some((why, true)),
            }
        }
        Done::Said(said) => {
            studies.working = studies.working.saturating_sub(1);
            studies.read_at = f64::NEG_INFINITY;
            match said {
                Ok(words) if words.is_empty() => {}
                Ok(words) => studies.last = Some((words, false)),
                Err(why) => studies.last = Some((why, true)),
            }
        }
        Done::Started(started) => {
            studies.working = studies.working.saturating_sub(1);
            studies.read_at = f64::NEG_INFINITY;
            match started {
                Ok(_) => {
                    studies.dialog.open = false;
                    studies.dialog.of.clear();
                    studies.dialog.refused = None;
                }
                Err(why) => studies.dialog.refused = Some(why),
            }
        }
        Done::Asked(asked) => {
            studies.working = studies.working.saturating_sub(1);
            match asked {
                Ok(question) => studies.question = Some(question),
                Err(why) => studies.last = Some((why, true)),
            }
        }
        Done::Sounding(asked, sounding) => {
            if studies.preparing != Some(asked) {
                // Asked for before the selection moved on: not what is wanted now.
                return;
            }
            studies.preparing = None;
            match sounding {
                Ok(sounding) => {
                    cx.session.lock().audition_round(sounding.audio, 0.0, sounding.beats);
                    studies.sounding = Some(asked);
                }
                Err(why) => studies.last = Some((why, true)),
            }
        }
        Done::Picked(picked) => {
            studies.dialog.picking = false;
            if let Some(file) = picked {
                studies.dialog.of = file.display().to_string();
            }
        }
        Done::Words(id, words) => {
            studies.working = studies.working.saturating_sub(1);
            match words {
                Ok(lyrics) => {
                    if let Some(open) = studies.open.as_ref().filter(|open| open.id == id) {
                        let again = Open {
                            id: open.id.clone(),
                            title: open.title.clone(),
                            seconds: open.seconds,
                            stems: open.stems.clone(),
                            heard: open.heard.clone(),
                            lyrics: Some(lyrics),
                            outlines: open.outlines.clone(),
                            bars: open.bars.clone(),
                            key: open.key,
                            corrections: open.corrections.clone(),
                            of: open.of.clone(),
                        };
                        studies.open = Some(Arc::new(again));
                    }
                }
                Err(why) => studies.last = Some((why, true)),
            }
        }
        Done::Taken(taken) => {
            studies.working = studies.working.saturating_sub(1);
            match taken {
                Ok(Some(clip)) => cx.state.select_clips(vec![clip]),
                Ok(None) => {}
                Err(why) => cx.state.toasts.error(why),
            }
        }
    }
}

/// What the workspace is to show of a study once it is open: for a
/// picture, which cannot drag.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shown {
    /// Bars to select, first and last from 1, across every lane.
    pub bars: Option<(usize, usize)>,
    /// The lanes the selection covers, by name; none for all of them.
    pub lanes: Vec<String>,
    pub side: Option<Side>,
    /// Looked at close: a few bars around the selection.
    pub close: bool,
}

/// Lays the view out as `shown` asks.
fn arrange(view: &mut View, open: &Open, shown: &Shown) {
    if let Some(side) = shown.side {
        view.side = side;
    }
    let Some((first, last)) = shown.bars else { return };
    let line = |bar: usize| open.bars.get(bar.saturating_sub(1)).copied();
    let (Some(from), Some(to)) = (line(first), line(last + 1)) else { return };
    let named: Vec<usize> = open
        .stems
        .iter()
        .enumerate()
        .filter(|(_, (name, _, _))| shown.lanes.contains(name))
        .map(|(index, _)| index)
        .collect();
    let lanes = match (named.first(), named.last()) {
        (Some(first), Some(last)) => (*first, *last),
        _ => (0, open.stems.len().saturating_sub(1)),
    };
    view.selection = Some(Selection { from, to, lanes });
    if shown.close {
        // About three times the selection, with it in the middle.
        let length = to - from;
        (view.from, view.per_second) = ((from - length).max(0.0), 1000.0 / (3.0 * length).max(1.0));
    }
}

/// Opens the study `id` in the workspace: read by a helper, shown when
/// it has been.
pub fn open(ctx: &egui::Context, cx: &mut Cx, id: &str) {
    cx.state.detail_tab = super::state::DetailTab::Study;
    if cx.studies.opening.as_deref() == Some(id) {
        return;
    }
    cx.studies.opening = Some(id.to_owned());
    cx.studies.last = None;
    stop_sounding(cx);
    let id = id.to_owned();
    cx.studies.spawn(ctx, &cx.sounds.runtime, move |runtime, report| report(Done::Opened(read(runtime, &id))));
}

/// Does something that ends in a sentence, on a thread of its own.
fn act(ctx: &egui::Context, cx: &mut Cx, job: impl FnOnce(&Runtime) -> Result<String, String> + Send + 'static) {
    cx.studies.working += 1;
    cx.studies.last = None;
    cx.studies.spawn(ctx, &cx.sounds.runtime, |runtime, report| report(Done::Said(job(runtime))));
}

/// Asks for a study of what the dialog names: a file, or a link.
pub fn start(ctx: &egui::Context, cx: &mut Cx) {
    let of = cx.studies.dialog.of.trim().trim_matches('"').to_owned();
    if of.is_empty() {
        cx.studies.dialog.refused = Some("Say what to study: a file, or a link to a recording.".to_owned());
        return;
    }
    let link = of.starts_with("http://") || of.starts_with("https://");
    let options = StudyOptions {
        words: cx.studies.dialog.words,
        stems: if cx.studies.dialog.best_stems { trill_study::StemChoice::Best } else { StudyOptions::default().stems },
        ..StudyOptions::default()
    };
    cx.studies.working += 1;
    cx.studies.dialog.refused = None;
    cx.studies.spawn(ctx, &cx.sounds.runtime, move |runtime, report| {
        let started = runtime.studies().and_then(|desk| {
            let source =
                if link { Source::Link { url: of, by: None } } else { Source::File { path: PathBuf::from(of) } };
            desk.start(source, options, None).map(|study| study.id)
        });
        report(Done::Started(started));
    });
}

/// The box that picks the file to study.
pub fn pick_file(ctx: &egui::Context, cx: &mut Cx) {
    if std::mem::replace(&mut cx.studies.dialog.picking, true) {
        return;
    }
    cx.studies.spawn(ctx, &cx.sounds.runtime, |_, report| {
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("A recording to study")
            .add_filter("Audio", trill_media::AUDIO_EXTENSIONS);
        report(Done::Picked(pollster::block_on(dialog.pick_file()).map(|file| file.path().to_path_buf())));
    });
}

pub fn stop(ctx: &egui::Context, cx: &mut Cx, id: String) {
    act(ctx, cx, move |runtime| runtime.studies()?.stop(&id).map(|_| String::new()));
}

pub fn resume(ctx: &egui::Context, cx: &mut Cx, id: String) {
    act(ctx, cx, move |runtime| runtime.studies()?.resume(&id).map(|_| String::new()));
}

/// Asks to remove a study, or with `stems` only its stems: what plays its
/// files is looked up first, and the question that follows names it.
pub fn ask_to_remove(ctx: &egui::Context, cx: &mut Cx, id: String, title: String, stems: bool) {
    cx.studies.working += 1;
    cx.studies.spawn(ctx, &cx.sounds.runtime, move |runtime, report| {
        let asked = runtime.study_used_by(&id, stems).map(|used| Question { id, title, stems, used });
        report(Done::Asked(asked));
    });
}

/// Removes what a question was about, once it was answered with yes.
pub fn remove(ctx: &egui::Context, cx: &mut Cx, question: Question) {
    if cx.studies.open.as_ref().is_some_and(|open| open.id == question.id) && !question.stems {
        cx.studies.open = None;
        stop_sounding(cx);
    }
    // Its lanes are read again once the stems are gone: they then say so.
    let reopen = cx.studies.open.as_ref().is_some_and(|open| open.id == question.id);
    cx.studies.working += 1;
    cx.studies.last = None;
    cx.studies.spawn(ctx, &cx.sounds.runtime, move |runtime, report| {
        let removed = if question.stems {
            runtime.drop_study_stems(&question.id).map(|freed| {
                format!(
                    "The stems of {} are gone ({} MB); what was found in them is kept.",
                    question.title,
                    freed / (1 << 20)
                )
            })
        } else {
            runtime
                .studies()
                .and_then(|desk| desk.delete(&question.id))
                .map(|_| format!("{} is removed.", question.title))
        };
        let gone = removed.is_ok();
        report(Done::Said(removed));
        if reopen && question.stems && gone {
            report(Done::Opened(read(runtime, &question.id)));
        }
    });
}

/// Reads the grid of the open study another way, or with nothing the way
/// it was found: everything that follows from the grid is heard again.
pub fn correct(ctx: &egui::Context, cx: &mut Cx, corrections: Vec<Correction>) {
    let Some(id) = cx.studies.open.as_ref().map(|open| open.id.clone()) else { return };
    stop_sounding(cx);
    cx.studies.view.selection = None;
    cx.studies.working += 1;
    cx.studies.opening = Some(id.clone());
    cx.studies.spawn(ctx, &cx.sounds.runtime, move |runtime, report| {
        let corrected = runtime.studies().and_then(|desk| {
            desk.correct(&id, corrections)?;
            desk.wait(&id, std::time::Duration::from_secs(300)).map(|_| ())
        });
        report(Done::Said(corrected.map(|()| String::new())));
        report(Done::Opened(read(runtime, &id)));
    });
}

/// Puts the word that is being typed over in its place.
pub fn correct_word(ctx: &egui::Context, cx: &mut Cx) {
    let Some((line, word, text)) = cx.studies.view.word.take() else { return };
    let Some(open) = cx.studies.open.clone() else { return };
    let was =
        open.lyrics.as_ref().and_then(|lyrics| lyrics.lines.get(line)?.words.get(word)).map(|word| word.text.trim());
    if was == Some(text.trim()) || text.trim().is_empty() {
        return;
    }
    cx.studies.working += 1;
    let id = open.id.clone();
    cx.studies.spawn(ctx, &cx.sounds.runtime, move |runtime, report| {
        report(Done::Words(id.clone(), runtime.correct_word(&id, line, word, &text)));
    });
}

/// The stems of the open study that are heard: the lanes of the selection
/// (or all of them), less the ones that are muted, or only the soloed.
pub fn heard_stems(open: &Open, view: &View) -> Vec<String> {
    let (first, last) =
        view.selection.as_ref().map_or((0, open.stems.len().saturating_sub(1)), |selection| selection.lanes);
    open.stems
        .iter()
        .enumerate()
        .filter(|(index, _)| (first..=last).contains(index))
        .map(|(_, (name, _, _))| name)
        .filter(|name| if view.solo.is_empty() { !view.muted.contains(*name) } else { view.solo.contains(*name) })
        .cloned()
        .collect()
}

/// The selection of the workspace as a stretch to take: what is heard is
/// what would be taken.
pub fn selected(cx: &Cx) -> Option<TakeStretch> {
    let open = cx.studies.open.as_ref()?;
    let view = &cx.studies.view;
    let selection = view.selection.as_ref()?;
    let stems = heard_stems(open, view);
    if stems.is_empty() {
        return None;
    }
    let mut take = TakeStretch::new(open.id.clone(), stems, selection.from, selection.to);
    take.as_recorded = view.as_recorded;
    take.pitch = if view.to_song_key && !view.as_recorded { Pitch::SongKey } else { Pitch::AsRecorded };
    take.at = cx.state.arrangement.cursor;
    Some(take)
}

/// How long a stretch will be in the song, in beats.
pub fn beats_of(cx: &Cx, open: &Open, take: &TakeStretch) -> f64 {
    if take.as_recorded {
        (take.to - take.from) * cx.snap.project.tempo_map().tempo_at(take.at) / 60.0
    } else {
        let grid = &open.heard.grid;
        let beats = grid.beat_at(take.to) - grid.beat_at(take.from);
        if (beats - beats.round()).abs() < 0.05 { beats.round() } else { beats }
    }
}

/// Plays `take` round as it would sound in the song, or stops what plays.
pub fn sound(ctx: &egui::Context, cx: &mut Cx, take: TakeStretch) {
    cx.studies.asked += 1;
    let asked = cx.studies.asked;
    cx.studies.preparing = Some(asked);
    cx.studies.last = None;
    cx.studies.spawn(ctx, &cx.sounds.runtime, move |runtime, report| {
        let sounding = runtime.study(&take.study).and_then(|studied| runtime.hear_stretch(&studied, &take));
        report(Done::Sounding(asked, sounding));
    });
}

/// Plays the selection round, or stops it if it plays.
pub fn toggle_sounding(ctx: &egui::Context, cx: &mut Cx) {
    if cx.studies.is_sounding() {
        return stop_sounding(cx);
    }
    match selected(cx) {
        Some(take) => sound(ctx, cx, take),
        None => cx.studies.last = Some(("Select a stretch across the lanes first: drag over them.".to_owned(), false)),
    }
}

pub fn stop_sounding(cx: &mut Cx) {
    if cx.studies.sounding.take().is_some() {
        cx.session.lock().stop_audition();
    }
    cx.studies.preparing = None;
}

/// Plays one hit of the open study, once.
pub fn sound_hit(ctx: &egui::Context, cx: &mut Cx, id: String) {
    let Some(open) = cx.studies.open.clone() else { return };
    stop_sounding(cx);
    let study = open.id.clone();
    act(ctx, cx, move |runtime| {
        let studied = runtime.study(&study)?;
        let heard = studied.heard.clone().ok_or("the study is not done")?;
        let hit = heard.one_shots.iter().find(|hit| hit.id == id).ok_or("that hit is not in the study")?;
        let file = runtime.hit_file(&studied, hit)?;
        runtime.preview(&file, 0.0).map(|_| String::new()).map_err(|error| error.to_string())
    });
}

/// Takes what was dragged from a study to where it was let go.
pub fn land(ctx: &egui::Context, cx: &mut Cx, drag: StudyDrag, target: DropTarget) {
    let lane_of = |target: &DropTarget| match target {
        DropTarget::Lane { track, beat } => Some((Lane::Track(*track), *beat)),
        DropTarget::NewTrack { beat, .. } => Some((Lane::New, *beat)),
        _ => None,
    };
    cx.studies.working += 1;
    cx.studies.spawn(ctx, &cx.sounds.runtime, move |runtime, report| {
        let taken = match (drag, lane_of(&target)) {
            (StudyDrag::Stretch(mut take, _), Some((lane, at))) => {
                (take.lane, take.at) = (lane, at);
                runtime.take_stretch(&take).map(|taken| taken.clips.first().copied())
            }
            (StudyDrag::Notes { study, part, from, to, .. }, Some((lane, at))) => {
                let take = TakeNotes { study, part, from, to, lane, at, quantize: None, instrument: None };
                runtime.take_notes(&take, None).map(|taken| Some(taken.clip))
            }
            (StudyDrag::Hit { study, id }, lane) => {
                let to = match (lane, &target) {
                    (Some((lane, at)), _) => Some(HitTo::Lane(lane, at)),
                    (None, DropTarget::Pad { device, note }) => Some(HitTo::Pad(*device, Some(*note))),
                    (None, DropTarget::Rack { device }) => Some(HitTo::Pad(*device, None)),
                    (None, DropTarget::Sampler { device }) => Some(HitTo::Sampler(*device)),
                    _ => None,
                };
                match to {
                    Some(to) => runtime.take_hit(&study, &id, to).map(|taken| taken.clip),
                    None => Err("That cannot be dropped there".to_owned()),
                }
            }
            _ => Err("That cannot be dropped there".to_owned()),
        };
        report(Done::Taken(taken));
    });
}

/// Sends the selection to the song without a drag: onto the selected
/// track at the edit cursor, or a track of its own.
pub fn to_the_song(ctx: &egui::Context, cx: &mut Cx) {
    let Some(take) = selected(cx) else {
        cx.studies.last = Some(("Select a stretch across the lanes first: drag over them.".to_owned(), false));
        return;
    };
    let Some(open) = cx.studies.open.clone() else { return };
    let beats = beats_of(cx, &open, &take);
    let beat = cx.state.arrangement.cursor;
    let selected = cx.state.selected_track.and_then(|id| cx.snap.track(id));
    let target = match selected.filter(|track| track.kind == trill_core::TrackKind::Normal && !cx.state.master_selected)
    {
        Some(track) => DropTarget::Lane { track: track.id, beat },
        None => DropTarget::NewTrack { beat, colors: super::dragging::new_track_colors(cx, 1) },
    };
    land(ctx, cx, StudyDrag::Stretch(take, beats), target);
}

/// Takes something of the open study that is not a stretch: its tempo, its
/// parts as markers, its key, the recording as a reference, its kit.
pub fn take(ctx: &egui::Context, cx: &mut Cx, what: Whole) {
    let Some(id) = cx.studies.open.as_ref().map(|open| open.id.clone()) else { return };
    let at = cx.snap.project.meter_map().bar_at(cx.state.arrangement.cursor).start;
    act(ctx, cx, move |runtime| match what {
        Whole::Tempo => runtime.take_tempo(&id, at).map(|taken| {
            if taken.tempos == 1 {
                format!(
                    "The song has the recording\u{2019}s tempo from there: {} bars at {:.1}.",
                    taken.bars, taken.slowest
                )
            } else {
                format!(
                    "The song has the recording\u{2019}s tempo from there, bar by bar: {} bars, {:.1} to {:.1}.",
                    taken.bars, taken.slowest, taken.fastest
                )
            }
        }),
        Whole::Parts => {
            runtime.take_parts(&id, at).map(|markers| format!("{} markers are in the song.", markers.len()))
        }
        Whole::Reference => runtime
            .take_reference(&id, at, false)
            .map(|_| "The recording is beside the song: listen to it on its track.".to_owned()),
        Whole::Kit => {
            runtime.take_kit(&id, None).map(|kit| format!("A drum rack of {} hits is in the song.", kit.pads.len()))
        }
        Whole::Key => {
            let studied = runtime.study(&id)?;
            let key = studied.heard.as_deref().and_then(taking::key_of).ok_or("No key was read in this recording.")?;
            runtime
                .session
                .lock()
                .edit(|project| {
                    project.set_key(Some(key));
                    Ok(())
                })
                .map_err(|error| error.to_string())?;
            Ok(format!("The song is said to be in {}.", key.name()))
        }
    });
}

/// What is taken of a study as a whole.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Whole {
    Tempo,
    Parts,
    Reference,
    Kit,
    Key,
}
