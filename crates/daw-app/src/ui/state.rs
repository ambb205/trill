//! View state that belongs to the window rather than the project (selection,
//! zoom, which tab is open) and [`Cx`], the bundle every view is handed.

use super::files::Jobs;
use super::snapshot::{Live, Snapshot};
use super::timeline::{PitchView, Snap, TimeView};
use super::toast::Toasts;
use daw_core::device::DeviceDescriptor;
use daw_core::{Beats, ChainRef, ClipId, DeviceId, EditError, Note, Project, TrackId};
use daw_engine::{Session, SessionError, SharedSession};
use daw_noir::Theme;
use eframe::egui;
use std::time::{Duration, Instant};

/// The tabs of the detail panel.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum DetailTab {
    /// The piano roll of the selected clip.
    #[default]
    Clip,
    /// The device chain of the selected track, and the master's.
    Devices,
}

/// Which pane keyboard commands such as Delete apply to: the one the user
/// last clicked in.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum FocusArea {
    #[default]
    Arrangement,
    PianoRoll,
}

/// What a drag in the arrangement is doing.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ArrangeDrag {
    /// `grab_offset` is how far into the clip it was grabbed, so the clip
    /// does not jump to the pointer.
    MoveClip {
        clip: ClipId,
        grab_offset: Beats,
    },
    ResizeClip {
        clip: ClipId,
    },
    /// Scrubbing the playhead along the ruler.
    Seek,
    MoveLoop {
        grab_offset: Beats,
    },
    /// Dragging one end of the loop region; `anchor` is the end that stays.
    StretchLoop {
        anchor: Beats,
    },
}

pub struct ArrangementState {
    pub time: TimeView,
    /// Vertical scroll of the track list, in pixels.
    pub scroll_y: f32,
    pub drag: Option<ArrangeDrag>,
    /// False until the view has been zoomed to show the whole song, which
    /// happens once per project.
    pub framed: bool,
}

/// What a drag in the piano roll's note grid is doing.
#[derive(Clone, Debug, PartialEq)]
pub enum NoteDrag {
    /// Moving the selected notes. `originals` are the notes as they were
    /// when the drag began and `current` as they are in the project now;
    /// every step is computed from the originals so rounding cannot build up.
    Move {
        originals: Vec<Note>,
        current: Vec<Note>,
        grabbed: Note,
        grab_beat: Beats,
    },
    Resize {
        originals: Vec<Note>,
        current: Vec<Note>,
        grabbed: Note,
    },
    /// Rubber-band selection, from the screen position where it began.
    /// `kept` is the selection it adds to (Shift held), else empty.
    Marquee {
        origin: egui::Pos2,
        kept: Vec<Note>,
    },
    /// Dragging a stem in the velocity lane. The selected notes follow the
    /// grabbed one by the same amount.
    Velocity {
        originals: Vec<Note>,
        current: Vec<Note>,
        grabbed: Note,
    },
}

pub struct PianoRollState {
    pub time: TimeView,
    pub pitch: PitchView,
    pub snap: Snap,
    pub drag: Option<NoteDrag>,
    /// The clip the view was last framed on, so selecting another clip
    /// reframes once and then leaves the user's zoom alone.
    pub framed: Option<ClipId>,
    /// Length given to new notes: the last length the user drew or resized.
    pub note_length: Beats,
}

/// A note being auditioned through the selected track's instrument.
#[derive(Copy, Clone, Debug)]
pub struct Audition {
    pub track: TrackId,
    pub pitch: u8,
    /// When to release it; `None` while a key or mouse button holds it.
    pub release_at: Option<Instant>,
}

/// The device browser down the left side.
pub struct BrowserState {
    pub open: bool,
    /// What is typed in the search field; narrows the lists.
    pub search: String,
}

/// The one-line hint in the status bar: what the thing under the pointer is
/// or does.
///
/// Views ask for a hint while they are hovered; the status bar, which is
/// laid out before them, shows what was asked for on the previous frame.
#[derive(Default)]
pub struct Hint {
    shown: String,
    asked: Option<String>,
}

impl Hint {
    /// Asks for `text` to be shown. The last caller of a frame wins, which
    /// is the innermost thing under the pointer.
    pub fn set(&mut self, text: impl Into<String>) {
        self.asked = Some(text.into());
    }

    /// The hint to show this frame; empty when nothing asked for one.
    pub fn text(&self) -> &str {
        &self.shown
    }

    /// Call once the views have run. Returns true if the hint changed, in
    /// which case another frame is needed to show it.
    pub fn end_frame(&mut self) -> bool {
        let next = self.asked.take().unwrap_or_default();
        let changed = next != self.shown;
        self.shown = next;
        changed
    }
}

/// A name being typed. The project is only edited when the user commits,
/// so a rename is one undo step rather than one per keystroke.
#[derive(Clone, Debug, PartialEq)]
pub struct TextEditState<Target> {
    pub target: Target,
    pub text: String,
    /// False until the field has been given keyboard focus once.
    pub focused: bool,
}

pub struct UiState {
    pub selected_track: Option<TrackId>,
    /// True while the master is what the Devices tab shows and the browser
    /// adds to, rather than the selected track.
    pub master_selected: bool,
    pub selected_clip: Option<ClipId>,
    /// Selected notes of the selected clip, by value: notes have no ids.
    pub selected_notes: Vec<Note>,
    /// The device whose presets the browser lists; one of the chain the
    /// Devices tab shows.
    pub selected_device: Option<DeviceId>,
    pub detail_tab: DetailTab,
    pub focus: FocusArea,
    pub arrangement: ArrangementState,
    pub piano: PianoRollState,
    pub renaming_track: Option<TextEditState<TrackId>>,
    pub renaming_clip: Option<TextEditState<ClipId>>,
    pub audition: Option<Audition>,
    pub browser: BrowserState,
    pub hint: Hint,
    /// Name of a theme chosen from the View menu; the app switches to it
    /// once the frame is over.
    pub theme_request: Option<&'static str>,
    pub toasts: Toasts,
    /// True between the first edit of a drag and the mouse button's release.
    pub gesture_open: bool,
    /// Set when the user dismisses the master's clip indicator.
    pub clear_clip_indicator: bool,
}

impl UiState {
    pub fn new(detail_tab: DetailTab) -> Self {
        Self {
            selected_track: None,
            master_selected: false,
            selected_clip: None,
            selected_notes: Vec::new(),
            selected_device: None,
            detail_tab,
            focus: FocusArea::default(),
            arrangement: ArrangementState { time: TimeView::new(22.0), scroll_y: 0.0, drag: None, framed: false },
            piano: PianoRollState {
                time: TimeView::new(48.0),
                pitch: PitchView { top: 84.0, row_height: 12.0 },
                snap: Snap::Sixteenth,
                drag: None,
                framed: None,
                note_length: 0.25,
            },
            renaming_track: None,
            renaming_clip: None,
            audition: None,
            browser: BrowserState { open: true, search: String::new() },
            hint: Hint::default(),
            theme_request: None,
            toasts: Toasts::default(),
            gesture_open: false,
            clear_clip_indicator: false,
        }
    }

    pub fn select_track(&mut self, track: TrackId) {
        if self.selected_track != Some(track) || self.master_selected {
            self.selected_device = None;
        }
        self.selected_track = Some(track);
        self.master_selected = false;
    }

    /// Makes the master the target of the Devices tab and the browser.
    pub fn select_master(&mut self) {
        if !self.master_selected {
            self.selected_device = None;
        }
        self.master_selected = true;
    }

    /// The chain the Devices tab shows and the browser adds to: the
    /// master's when it is selected or there is no track, else the selected
    /// track's.
    pub fn shown_chain(&self) -> ChainRef {
        match self.selected_track {
            Some(track) if !self.master_selected => ChainRef::Track(track),
            _ => ChainRef::Master,
        }
    }

    /// Selects a clip and the track it is on.
    pub fn select_clip(&mut self, track: TrackId, clip: ClipId) {
        self.select_track(track);
        if self.selected_clip != Some(clip) {
            self.selected_clip = Some(clip);
            self.selected_notes.clear();
        }
    }

    pub fn clear_clip_selection(&mut self) {
        self.selected_clip = None;
        self.selected_notes.clear();
    }

    /// Drops selections that point at things that no longer exist, after
    /// the project changed under us (undo, an edit from another front end).
    pub fn reconcile(&mut self, project: &Project) {
        if self.selected_track.is_some_and(|id| project.track(id).is_err()) {
            self.selected_track = None;
        }
        match self.selected_clip.map(|id| project.clip(id)) {
            Some(Ok(clip)) => {
                let notes = clip.notes();
                self.selected_notes.retain(|selected| notes.contains(selected));
            }
            Some(Err(_)) => self.clear_clip_selection(),
            None => self.selected_notes.clear(),
        }
        if self.selected_track.is_none() {
            self.selected_track = project.tracks.first().map(|track| track.id);
        }
        // The selected device is always one of the shown chain; by default
        // its first, which on a track is the instrument.
        let chain = project.chain(self.shown_chain()).map(Vec::as_slice).unwrap_or_default();
        if !self.selected_device.is_some_and(|id| chain.iter().any(|device| device.id == id)) {
            self.selected_device = chain.first().map(|device| device.id);
        }
    }

    /// Forgets everything tied to the previous project.
    pub fn reset_for_new_project(&mut self) {
        let fresh = Self::new(self.detail_tab);
        let toasts = std::mem::take(&mut self.toasts);
        let browser = BrowserState { open: self.browser.open, search: String::new() };
        *self = Self { toasts, browser, ..fresh };
    }
}

/// The audio output, as it was when the engine started.
pub struct AudioSummary {
    /// Name and sample rate of the output, as the engine describes it.
    pub output: String,
    pub sample_rate: f64,
    /// Set when no audio device could be opened and nothing will be heard.
    pub warning: Option<String>,
}

/// How long an auditioned note sounds when triggered by a click.
pub const AUDITION_LENGTH: Duration = Duration::from_millis(220);

/// Everything a view needs: the project as of this frame, the live values,
/// the window's state, and the session for making edits.
pub struct Cx<'a> {
    pub session: &'a SharedSession,
    pub snap: &'a Snapshot,
    pub live: &'a Live,
    /// The colors, type and metrics everything is painted with.
    pub theme: &'a Theme,
    pub descriptors: &'a [DeviceDescriptor],
    /// False until the plugin scan has delivered the full device catalog.
    pub catalog_ready: bool,
    pub audio: &'a AudioSummary,
    /// Address of the MCP server, when one is running for this window.
    pub mcp_url: Option<&'a str>,
    pub state: &'a mut UiState,
    pub jobs: &'a mut Jobs,
}

impl Cx<'_> {
    /// Applies an edit to the project as one undo step. A failed edit
    /// leaves the project untouched and tells the user why.
    pub fn edit<T>(&mut self, edit: impl FnOnce(&mut Project) -> Result<T, EditError>) -> Option<T> {
        let result = self.session.lock().edit(edit);
        self.report(result)
    }

    /// Runs a fallible session operation, reporting failure to the user.
    pub fn try_session<T>(&mut self, call: impl FnOnce(&mut Session) -> Result<T, SessionError>) -> Option<T> {
        let result = call(&mut self.session.lock());
        self.report(result)
    }

    /// Runs an infallible session operation (transport, undo).
    pub fn with_session<T>(&self, call: impl FnOnce(&mut Session) -> T) -> T {
        call(&mut self.session.lock())
    }

    fn report<T>(&mut self, result: Result<T, SessionError>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.state.toasts.error(error.to_string());
                None
            }
        }
    }

    /// Shows `text` in the status bar while the pointer is over `response`.
    pub fn hint(&mut self, response: &egui::Response, text: impl Into<String>) {
        if response.hovered() || response.dragged() {
            self.state.hint.set(text);
        }
    }

    /// Call when a drag starts editing. Until the mouse button is released
    /// all edits collapse into one undo step; the app closes the gesture, so
    /// a widget that disappears mid-drag cannot leave one open.
    pub fn begin_gesture(&mut self) {
        if !self.state.gesture_open {
            self.session.lock().begin_gesture();
            self.state.gesture_open = true;
        }
    }

    /// Sounds a note on `track` until `release_at`, or until
    /// [`Self::stop_audition`] when `None`.
    pub fn audition(&mut self, track: TrackId, pitch: u8, velocity: u8, release_at: Option<Instant>) {
        self.stop_audition();
        self.session.lock().note_on(track, pitch, velocity);
        self.state.audition = Some(Audition { track, pitch, release_at });
    }

    pub fn stop_audition(&mut self) {
        if let Some(audition) = self.state.audition.take() {
            self.session.lock().note_off(audition.track, audition.pitch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(pitch: u8, start: Beats) -> Note {
        Note { pitch, start, length: 1.0, velocity: 100 }
    }

    #[test]
    fn selecting_another_clip_clears_the_note_selection() {
        let mut state = UiState::new(DetailTab::Clip);
        state.select_clip(TrackId(1), ClipId(2));
        state.selected_notes.push(note(60, 0.0));
        state.select_clip(TrackId(1), ClipId(2));
        assert_eq!(state.selected_notes.len(), 1, "reselecting the same clip keeps its notes selected");
        state.select_clip(TrackId(3), ClipId(4));
        assert!(state.selected_notes.is_empty());
        assert_eq!((state.selected_track, state.selected_clip), (Some(TrackId(3)), Some(ClipId(4))));
    }

    #[test]
    fn reconcile_drops_what_no_longer_exists() {
        let mut project = Project::new("t");
        let keep = project.add_track("Keep");
        let doomed = project.add_track("Doomed");
        let clip = project.add_midi_clip(doomed, "c", 0.0, 4.0).unwrap();
        project.clip_mut(clip).unwrap().add_notes(&[note(60, 0.0), note(62, 1.0)]).unwrap();

        let mut state = UiState::new(DetailTab::Clip);
        state.select_clip(doomed, clip);
        state.selected_notes = vec![note(60, 0.0), note(99, 9.0)];
        state.reconcile(&project);
        assert_eq!(state.selected_notes, [note(60, 0.0)], "a note that was edited away is deselected");
        assert_eq!(state.selected_clip, Some(clip));

        project.remove_track(doomed).unwrap();
        state.reconcile(&project);
        assert_eq!(state.selected_clip, None);
        assert!(state.selected_notes.is_empty());
        assert_eq!(state.selected_track, Some(keep), "falls back to the first track");

        project.remove_track(keep).unwrap();
        state.reconcile(&project);
        assert_eq!(state.selected_track, None);
    }

    #[test]
    fn the_shown_chain_follows_the_selection() {
        let mut project = Project::new("t");
        let limiter = project.add_device(ChainRef::Master, "builtin:limiter", "Limiter", None).unwrap();
        let mut state = UiState::new(DetailTab::Devices);
        state.reconcile(&project);
        assert_eq!(state.shown_chain(), ChainRef::Master, "with no tracks there is only the master");
        assert_eq!(state.selected_device, Some(limiter));

        let track = project.add_track("A");
        let synth = project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
        let delay = project.add_device(ChainRef::Track(track), "builtin:delay", "Delay", None).unwrap();
        state.reconcile(&project);
        assert_eq!(state.shown_chain(), ChainRef::Track(track));
        assert_eq!(state.selected_device, Some(synth), "the first device of a chain is selected by default");

        state.selected_device = Some(delay);
        state.reconcile(&project);
        assert_eq!(state.selected_device, Some(delay), "a valid choice is kept");

        state.select_master();
        state.reconcile(&project);
        assert_eq!((state.shown_chain(), state.selected_device), (ChainRef::Master, Some(limiter)));
        state.select_track(track);
        state.reconcile(&project);
        assert_eq!((state.shown_chain(), state.selected_device), (ChainRef::Track(track), Some(synth)));

        project.remove_device(synth).unwrap();
        state.reconcile(&project);
        assert_eq!(state.selected_device, Some(delay), "a removed device hands over to the first one left");
    }

    #[test]
    fn the_hint_shows_what_the_last_frame_asked_for() {
        let mut hint = Hint::default();
        assert_eq!(hint.text(), "");
        hint.set("outer");
        hint.set("inner");
        assert!(hint.end_frame(), "a new hint needs a frame to appear");
        assert_eq!(hint.text(), "inner", "the innermost caller wins");
        hint.set("inner");
        assert!(!hint.end_frame());
        assert!(hint.end_frame(), "nothing asked: the hint clears");
        assert_eq!(hint.text(), "");
    }

    #[test]
    fn a_new_project_resets_views_but_keeps_messages() {
        let mut state = UiState::new(DetailTab::Devices);
        state.select_clip(TrackId(1), ClipId(2));
        state.arrangement.time.origin = 64.0;
        state.toasts.info("saved");
        state.reset_for_new_project();
        assert_eq!(state.selected_clip, None);
        assert_eq!(state.arrangement.time.origin, 0.0);
        assert_eq!(state.detail_tab, DetailTab::Devices);
        assert_eq!(state.toasts.len(), 1);
        assert!(state.browser.open);

        state.browser.open = false;
        state.browser.search = "rev".to_owned();
        state.reset_for_new_project();
        assert!(!state.browser.open, "the browser stays as the user left it");
        assert!(state.browser.search.is_empty());
    }
}
