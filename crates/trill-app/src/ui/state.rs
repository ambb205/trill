//! View state that belongs to the window rather than the project (selection,
//! zoom, which tab is open) and [`Cx`], the bundle every view is handed.

use super::assistant::Assistant;
use super::audio_settings::Settings;
use super::editors::Editors;
use super::files::Jobs;
use super::keyboard::Keyboard;
use super::recovery::Recovery;
use super::snapshot::{Live, Snapshot};
use super::sounds::{DropTarget, Sounds};
use super::timeline::{Glide, GridChoice, PitchView, Snap, TimeView};
use super::toast::Toasts;
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use trill_core::device::DeviceDescriptor;
use trill_core::{
    AutomationPoint, AutomationTarget, Beats, ChainRef, Clip, ClipId, ControllerKind, DeviceId, EditError, Note,
    Project, TrackId, TrackKind,
};
use trill_engine::{AudioConfig, Session, SessionError, SharedSession, StreamInfo};
use trill_noir::Theme;

/// The tabs of the detail panel.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum DetailTab {
    /// The piano roll of the selected clip.
    #[default]
    Clip,
    /// The device chain of the selected track, and the master's.
    Devices,
    /// A channel strip for every track.
    Mixer,
}

/// Which pane keyboard commands such as Delete apply to: the one the user
/// last clicked in.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum FocusArea {
    #[default]
    Arrangement,
    PianoRoll,
    /// An automation lane; commands act on its selected points.
    Automation,
    /// The controller lane under the piano roll's notes; commands act on
    /// the selected points of the curve it shows.
    Controllers,
    /// The browser's list of sounds: arrows move through it, Enter and
    /// Space preview, and typing searches.
    Browser,
}

/// What the pointer does in the arrangement.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    /// Select, move and trim.
    #[default]
    Pointer,
    /// The pencil: writes automation in steps of the grid.
    Draw,
    /// The blade: a click splits the clip under it.
    Blade,
}

/// Where a clip was when a move began. Every step of the move is computed
/// from here, so rounding cannot build up along the way.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ClipOrigin {
    pub clip: ClipId,
    pub track: TrackId,
    pub start: Beats,
}

/// Which fade of an audio clip a handle sets.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FadeEnd {
    In,
    Out,
}

/// Where a dragged track header would land.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TrackDrop {
    /// The track it goes in front of; `None` puts it last.
    pub before: Option<TrackId>,
    /// The group it joins there.
    pub group: Option<TrackId>,
}

/// What a drag in an automation lane is doing. Points have no ids, so like
/// notes they are followed by value: `current` is what the last step left in
/// the project.
#[derive(Clone, Debug, PartialEq)]
pub enum LaneDrag {
    /// Moving the selected points. `press` is where the pointer went down.
    Points {
        originals: Vec<AutomationPoint>,
        current: Vec<AutomationPoint>,
        grabbed: AutomationPoint,
        press: egui::Pos2,
    },
    /// Moving a line by both of its ends, or bending it with Alt held.
    Segment { originals: [AutomationPoint; 2], current: [AutomationPoint; 2], press: egui::Pos2, bend: bool },
    /// Rubber-band selection, from the lane position where it began: a beat
    /// and a height in the lane (0 at the bottom, 1 at the top).
    Band { origin: (Beats, f32), kept: Vec<AutomationPoint> },
    /// The pencil. `last` is the grid cell it wrote on the previous frame,
    /// so a fast stroke leaves no cell out.
    Draw { last: Beats },
}

/// What a drag in the arrangement is doing.
#[derive(Clone, Debug, PartialEq)]
pub enum ArrangeDrag {
    /// Moving the selected clips together. `grab_offset` is how far into the
    /// grabbed clip the pointer is, so it does not jump to the pointer;
    /// `applied` is the shift in beats and lanes the project already has.
    MoveClips {
        grabbed: ClipId,
        grab_offset: Beats,
        origins: Vec<ClipOrigin>,
        applied: (Beats, i32),
    },
    TrimStart {
        clip: ClipId,
    },
    TrimEnd {
        clip: ClipId,
    },
    Fade {
        clip: ClipId,
        end: FadeEnd,
    },
    /// Rubber band over the lanes, from where it began: a beat and a height
    /// in the track list. `kept` is the selection it adds to (Shift held).
    Band {
        anchor: Beats,
        anchor_y: f32,
        kept: Vec<ClipId>,
    },
    /// Selecting time along the ruler.
    RulerSelect {
        anchor: Beats,
    },
    MoveLoop {
        grab_offset: Beats,
    },
    /// Dragging one end of the loop region, or drawing a new one; `anchor`
    /// is the end that stays.
    StretchLoop {
        anchor: Beats,
    },
    MoveMarker {
        index: usize,
    },
    /// Dragging a track header to another place in the list.
    MoveTrack {
        track: TrackId,
        drop: Option<TrackDrop>,
    },
    Lane {
        target: AutomationTarget,
        drag: LaneDrag,
    },
}

/// A stretch of the timeline picked out for looping, zooming or cutting.
#[derive(Clone, Debug, PartialEq)]
pub struct TimeSelection {
    pub start: Beats,
    pub end: Beats,
    /// The tracks whose lanes show it; empty when it was drawn on the ruler
    /// and spans them all.
    pub tracks: Vec<TrackId>,
}

/// Whose lane an automation envelope is shown in.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum LaneOwner {
    Track(TrackId),
    Master,
}

/// An automation lane the user has unfolded.
#[derive(Clone, Debug, PartialEq)]
pub struct LaneView {
    pub owner: LaneOwner,
    pub target: AutomationTarget,
    pub height: f32,
}

/// The selected points of one envelope, by value: points have no ids.
#[derive(Clone, Debug, PartialEq)]
pub struct PointSelection {
    pub target: AutomationTarget,
    pub points: Vec<AutomationPoint>,
}

pub struct ArrangementState {
    pub time: TimeView,
    /// A zoom-to-fit in progress; `time` follows it until it arrives.
    pub glide: Option<Glide>,
    /// Vertical scroll of the track list, in pixels.
    pub scroll_y: f32,
    pub drag: Option<ArrangeDrag>,
    /// False until the view has been zoomed to show the whole song, which
    /// happens once per project.
    pub framed: bool,
    pub grid: GridChoice,
    /// Whether edits land on the grid. Alt suspends it during a drag.
    pub snap: bool,
    /// Whether the view pages along with the playhead.
    pub follow: bool,
    /// The edit cursor: where pasting, splitting and new markers go.
    pub cursor: Beats,
    pub time_selection: Option<TimeSelection>,
    /// Height of a track nobody has resized: the vertical zoom.
    pub track_height: f32,
    /// Tracks dragged to a height of their own.
    pub heights: HashMap<TrackId, f32>,
    /// Group tracks whose members are folded away.
    pub folded: HashSet<TrackId>,
    pub lanes: Vec<LaneView>,
    pub points: Option<PointSelection>,
    pub tool: Tool,
    pub renaming_marker: Option<TextEditState<usize>>,
    /// What is typed in the lane chooser's search field.
    pub lane_search: String,
    /// A lane to bring into view once the layout knows where it is.
    pub reveal: Option<AutomationTarget>,
    /// Tracks and a stretch of time to bring into view likewise.
    pub show: Option<super::arrangement::Reveal>,
    /// The grid step and the width of the lanes as of the last frame, for
    /// commands that run before the arrangement is laid out: what arrow
    /// keys nudge by, and what "zoom to fit" fits into.
    pub step: Beats,
    pub lanes_width: f32,
}

impl ArrangementState {
    pub const DEFAULT_TRACK_HEIGHT: f32 = 60.0;
    pub const MIN_TRACK_HEIGHT: f32 = 26.0;
    pub const MAX_TRACK_HEIGHT: f32 = 240.0;
    pub const DEFAULT_LANE_HEIGHT: f32 = 58.0;
    pub const MIN_LANE_HEIGHT: f32 = 30.0;

    fn new() -> Self {
        Self {
            time: TimeView::new(22.0),
            glide: None,
            scroll_y: 0.0,
            drag: None,
            framed: false,
            grid: GridChoice::default(),
            snap: true,
            follow: true,
            cursor: 0.0,
            time_selection: None,
            track_height: Self::DEFAULT_TRACK_HEIGHT,
            heights: HashMap::new(),
            folded: HashSet::new(),
            lanes: Vec::new(),
            points: None,
            tool: Tool::default(),
            renaming_marker: None,
            lane_search: String::new(),
            reveal: None,
            show: None,
            step: 1.0,
            lanes_width: 900.0,
        }
    }

    /// Height of `track`'s row.
    pub fn height_of(&self, track: TrackId) -> f32 {
        self.heights.get(&track).copied().unwrap_or(self.track_height)
    }

    /// Whether `target`'s lane is unfolded.
    pub fn lane_open(&self, target: &AutomationTarget) -> bool {
        self.lanes.iter().any(|lane| lane.target == *target)
    }

    /// Unfolds a lane for `target` under `owner`, unless it is open already.
    pub fn open_lane(&mut self, owner: LaneOwner, target: AutomationTarget) {
        if !self.lane_open(&target) {
            self.lanes.push(LaneView { owner, target, height: Self::DEFAULT_LANE_HEIGHT });
        }
    }

    pub fn close_lane(&mut self, target: &AutomationTarget) {
        self.lanes.retain(|lane| lane.target != *target);
        if self.points.as_ref().is_some_and(|selection| selection.target == *target) {
            self.points = None;
        }
    }

    /// Zooms and scrolls towards `to` over a few frames, starting at `now`.
    pub fn glide_to(&mut self, to: TimeView, now: f64) {
        self.glide = (to != self.time).then_some(Glide { from: self.time, to, started: now });
    }
}

/// What copy and cut put aside for paste.
#[derive(Clone, Debug, PartialEq)]
pub enum Clipboard {
    Clips(Vec<CopiedClip>),
    /// Notes, with starts counted from the earliest of them.
    Notes(Vec<Note>),
}

/// A clip on the clipboard, with where it sat among the others copied
/// with it.
#[derive(Clone, Debug, PartialEq)]
pub struct CopiedClip {
    pub clip: Clip,
    /// Beats after the earliest copied clip.
    pub offset: Beats,
    /// Clip lanes below the topmost copied clip.
    pub lane: usize,
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

/// What the lane under the piano roll's notes shows.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum LaneShown {
    /// How hard each note is hit.
    #[default]
    Velocity,
    /// The clip's curve for one MIDI controller.
    Controller(ControllerKind),
}

pub struct PianoRollState {
    pub time: TimeView,
    pub pitch: PitchView,
    pub snap: Snap,
    pub drag: Option<NoteDrag>,
    /// What the lane under the notes shows. It stays as chosen from clip
    /// to clip: someone shaping expression goes on shaping expression.
    pub lane: LaneShown,
    /// The selected points of the curve the lane shows.
    pub lane_points: Vec<AutomationPoint>,
    /// What a drag in the controller lane is doing.
    pub lane_drag: Option<LaneDrag>,
    /// What is typed into the search of the lane's chooser.
    pub lane_search: String,
    /// The clip the view was last framed on, so selecting another clip
    /// reframes once and then leaves the user's zoom alone.
    pub framed: Option<ClipId>,
    /// Length given to new notes: the last length the user drew or resized.
    pub note_length: Beats,
    /// Velocity given to new notes: the last one set in the sidebar.
    pub note_velocity: u8,
    /// Where pasted notes go, in beats from the clip's start. Set by a
    /// click on the roll's ruler.
    pub cursor: Beats,
}

/// A note being auditioned through the selected track's instrument.
#[derive(Copy, Clone, Debug)]
pub struct Audition {
    pub track: TrackId,
    pub pitch: u8,
    /// When to release it; `None` while a key or mouse button holds it.
    pub release_at: Option<Instant>,
}

/// What the browser lists.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum BrowserTab {
    /// Instruments, effects, plugins and presets.
    #[default]
    Devices,
    /// The sound library.
    Sounds,
}

/// Whether the browser lists one-shots, loops or both.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum SoundForm {
    #[default]
    Any,
    OneShot,
    Loop,
}

/// The tempos the browser lists.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub enum TempoFilter {
    #[default]
    Any,
    /// Within a few beats per minute of the song's tempo.
    Song,
    /// From the first to the second, in beats per minute.
    Between(f32, f32),
}

/// The Sounds tab of the browser: what is searched for and what is chosen.
pub struct SoundBrowser {
    pub search: String,
    pub form: SoundForm,
    /// Only sounds of this kind: kicks, pads, vocals.
    pub tag: Option<trill_library::Tag>,
    pub tempo: TempoFilter,
    pub key: Option<trill_library::Key>,
    pub favorites: bool,
    /// Only sounds in this folder of the library or beneath it.
    pub folder: Option<PathBuf>,
    /// Folders of the tree that are unfolded.
    pub expanded: HashSet<PathBuf>,
    /// Whether the folder tree is shown at all.
    pub folders_open: bool,
    /// The chosen sound: the one the preview strip shows.
    pub selected: Option<PathBuf>,
    /// Where it is in the list, when it is in it.
    pub selected_row: Option<usize>,
    /// Play a sound as soon as it is chosen.
    pub auto_preview: bool,
    pub preview_gain_db: f32,
    /// Bring the chosen row into view on the next frame.
    pub reveal: bool,
    /// Give the search field the keyboard on the next frame.
    pub focus_search: bool,
    /// The folder the "add folder" dialog last added, to open beside it
    /// next time.
    pub last_added: Option<PathBuf>,
    /// Whether the search field had the keyboard on the last frame.
    pub search_focused: bool,
    /// Name of a sound to choose as soon as the list has it.
    pub pending_select: Option<String>,
}

impl Default for SoundBrowser {
    fn default() -> Self {
        Self {
            search: String::new(),
            form: SoundForm::Any,
            tag: None,
            tempo: TempoFilter::Any,
            key: None,
            favorites: false,
            folder: None,
            expanded: HashSet::new(),
            folders_open: true,
            selected: None,
            selected_row: None,
            auto_preview: true,
            preview_gain_db: -6.0,
            reveal: false,
            focus_search: false,
            last_added: None,
            search_focused: false,
            pending_select: None,
        }
    }
}

/// The browser down the left side.
pub struct BrowserState {
    pub open: bool,
    /// Its width as last laid out, in points.
    pub width: f32,
    /// Where it was on the last frame.
    pub rect: egui::Rect,
    pub tab: BrowserTab,
    /// What is typed in the devices' search field; narrows the lists.
    pub search: String,
    pub sounds: SoundBrowser,
}

impl BrowserState {
    pub const DEFAULT_WIDTH: f32 = 280.0;
    pub const MIN_WIDTH: f32 = 250.0;
    pub const MAX_WIDTH: f32 = 520.0;

    pub fn new() -> Self {
        Self {
            open: true,
            width: Self::DEFAULT_WIDTH,
            rect: egui::Rect::NOTHING,
            tab: BrowserTab::default(),
            search: String::new(),
            sounds: SoundBrowser::default(),
        }
    }
}

/// Where sounds being dragged came from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DragSource {
    /// A row of the browser, or its preview strip.
    Browser,
    /// Files dragged in from another program.
    System,
}

/// Sounds being dragged towards the song.
///
/// The views that can take them say so while the pointer is over them, by
/// setting `target`; the app carries the drop out when the button is let
/// go. Nothing is read from disk for any of this.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundDrag {
    pub source: DragSource,
    pub paths: Vec<PathBuf>,
    /// What to call it while it is in the air: the first file's name.
    pub label: String,
    /// Length of the first file in seconds, its tempo and what kind of
    /// sound it is, as far as the library knows: for drawing how long the
    /// clip will be.
    pub seconds: Option<f64>,
    pub tempo: Option<f64>,
    pub tags: trill_library::Tags,
    /// Where the pointer is; `None` when the system does not say (files
    /// from another program, on some platforms, until they are let go).
    pub pointer: Option<egui::Pos2>,
    /// Where it would land if let go now.
    pub target: Option<DropTarget>,
    /// Why it cannot be dropped where the pointer is, for the hint.
    pub refusal: Option<&'static str>,
}

impl SoundDrag {
    /// The length in beats the first clip would have on a track.
    pub fn beats(&self, project_tempo: f64, fallback: Beats) -> Beats {
        match self.seconds {
            Some(seconds) => super::sounds::dropped_length(seconds, self.tags, self.tempo, project_tempo),
            None => fallback,
        }
    }

    /// Offers `target` as the place the drag would land.
    pub fn offer(&mut self, target: DropTarget) {
        self.target = Some(target);
        self.refusal = None;
    }
}

/// Files let go over the window by another program, waiting for the system
/// to say where the pointer is.
#[derive(Clone, Debug, PartialEq)]
pub struct SystemDrop {
    pub paths: Vec<PathBuf>,
    /// Frames it has waited.
    pub waited: u32,
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
    /// The clip the Clip tab shows: the one selected last.
    pub selected_clip: Option<ClipId>,
    /// Every selected clip, `selected_clip` among them, in the order they
    /// were selected.
    pub selected_clips: Vec<ClipId>,
    /// Selected notes of the selected clip, by value: notes have no ids.
    pub selected_notes: Vec<Note>,
    /// The device whose presets the browser lists; one of the chain the
    /// Devices tab shows.
    pub selected_device: Option<DeviceId>,
    /// A device for the Devices tab to scroll its rack to.
    pub reveal_device: Option<DeviceId>,
    pub detail_tab: DetailTab,
    pub focus: FocusArea,
    pub arrangement: ArrangementState,
    pub piano: PianoRollState,
    pub renaming_track: Option<TextEditState<TrackId>>,
    pub renaming_clip: Option<TextEditState<ClipId>>,
    /// A clip being renamed on its own name strip in the arrangement.
    pub renaming_clip_inline: Option<TextEditState<InlineClip>>,
    pub audition: Option<Audition>,
    pub clipboard: Option<Clipboard>,
    /// Text for the system clipboard, set by copy and cut. A windowing
    /// backend only reports a paste while the system clipboard holds text,
    /// so the app leaves a note of what it copied there.
    pub copied_text: Option<String>,
    /// Whether the shortcut reference is up.
    pub shortcuts_open: bool,
    pub browser: BrowserState,
    /// Sounds on their way from the browser, or from another program, to
    /// wherever they are dropped.
    pub sound_drag: Option<SoundDrag>,
    pub system_drop: Option<SystemDrop>,
    /// The pad each drum rack's panel shows the controls of, by its note.
    pub selected_pads: HashMap<DeviceId, u8>,
    /// How much of its file the audio clip panel shows: the clip it was
    /// last framed on, and the seconds at its left and right edges.
    pub audio_view: Option<(ClipId, f64, f64)>,
    pub hint: Hint,
    /// Name of a theme chosen from the View menu; the app switches to it
    /// once the frame is over.
    pub theme_request: Option<&'static str>,
    pub toasts: Toasts,
    /// True between the first edit of a drag and the mouse button's release.
    pub gesture_open: bool,
    /// Set when the user dismisses the master's clip indicator.
    pub clear_clip_indicator: bool,
    /// How many projects this window has had open: it goes up each time
    /// another one takes the place of the open one.
    pub project_generation: u64,
}

impl UiState {
    pub fn new(detail_tab: DetailTab) -> Self {
        Self {
            selected_track: None,
            master_selected: false,
            selected_clip: None,
            selected_clips: Vec::new(),
            selected_notes: Vec::new(),
            selected_device: None,
            reveal_device: None,
            detail_tab,
            focus: FocusArea::default(),
            arrangement: ArrangementState::new(),
            piano: PianoRollState {
                time: TimeView::new(48.0),
                pitch: PitchView { top: 84.0, row_height: 12.0 },
                snap: Snap::Sixteenth,
                drag: None,
                lane: LaneShown::default(),
                lane_points: Vec::new(),
                lane_drag: None,
                lane_search: String::new(),
                framed: None,
                note_length: 0.25,
                note_velocity: super::piano_roll::DEFAULT_VELOCITY,
                cursor: 0.0,
            },
            renaming_track: None,
            renaming_clip: None,
            renaming_clip_inline: None,
            audition: None,
            clipboard: None,
            copied_text: None,
            shortcuts_open: false,
            browser: BrowserState::new(),
            sound_drag: None,
            system_drop: None,
            selected_pads: HashMap::new(),
            audio_view: None,
            hint: Hint::default(),
            theme_request: None,
            toasts: Toasts::default(),
            gesture_open: false,
            clear_clip_indicator: false,
            project_generation: 0,
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

    /// Selects one clip, and the track it is on.
    pub fn select_clip(&mut self, track: TrackId, clip: ClipId) {
        self.select_track(track);
        self.selected_clips = vec![clip];
        self.show_clip(Some(clip));
    }

    /// Adds a clip to the selection, or takes it out if it is in it.
    pub fn toggle_clip(&mut self, track: TrackId, clip: ClipId) {
        match self.selected_clips.iter().position(|selected| *selected == clip) {
            Some(index) => {
                self.selected_clips.remove(index);
                self.show_clip(self.selected_clips.last().copied());
            }
            None => {
                self.select_track(track);
                self.selected_clips.push(clip);
                self.show_clip(Some(clip));
            }
        }
    }

    /// Selects exactly `clips`; the last of them is the one the Clip tab
    /// shows.
    pub fn select_clips(&mut self, clips: Vec<ClipId>) {
        self.show_clip(clips.last().copied());
        self.selected_clips = clips;
    }

    /// Makes `clip` the one the Clip tab shows.
    fn show_clip(&mut self, clip: Option<ClipId>) {
        if self.selected_clip != clip {
            self.selected_clip = clip;
            self.selected_notes.clear();
            self.piano.lane_points.clear();
            self.piano.lane_drag = None;
        }
    }

    pub fn clear_clip_selection(&mut self) {
        self.selected_clips.clear();
        self.show_clip(None);
    }

    pub fn is_clip_selected(&self, clip: ClipId) -> bool {
        self.selected_clips.contains(&clip)
    }

    /// Drops selections that point at things that no longer exist, after
    /// the project changed under us (undo, an edit from another front end).
    pub fn reconcile(&mut self, project: &Project) {
        if self.selected_track.is_some_and(|id| project.track(id).is_err()) {
            self.selected_track = None;
        }
        self.selected_clips.retain(|id| project.clip(*id).is_ok());
        if self.selected_clip.is_some_and(|id| !self.selected_clips.contains(&id)) {
            self.show_clip(self.selected_clips.last().copied());
        }
        match self.selected_clip.map(|id| project.clip(id)) {
            Some(Ok(clip)) => {
                let notes = clip.notes();
                self.selected_notes.retain(|selected| notes.contains(selected));
                // The points of the curve the lane shows, as far as they
                // are still there.
                let curve = match self.piano.lane {
                    LaneShown::Controller(kind) => clip.controller(kind).map(|curve| curve.points.as_slice()),
                    LaneShown::Velocity => None,
                };
                self.piano.lane_points.retain(|selected| curve.is_some_and(|points| points.contains(selected)));
            }
            _ => {
                self.selected_notes.clear();
                self.piano.lane_points.clear();
            }
        }
        if self.selected_track.is_none() {
            self.selected_track = project.tracks.first().map(|track| track.id);
        }
        self.selected_pads.retain(|device, _| project.device(*device).is_ok());
        if self.audio_view.is_some_and(|(clip, ..)| self.selected_clip != Some(clip)) {
            self.audio_view = None;
        }
        self.reconcile_arrangement(project);
        self.resolve_device(project);
    }

    /// The arrangement's share of [`Self::reconcile`]: lanes, heights,
    /// folds and selections that named something now gone.
    fn reconcile_arrangement(&mut self, project: &Project) {
        let view = &mut self.arrangement;
        let exists = |id: &TrackId| project.track(*id).is_ok();
        view.heights.retain(|id, _| exists(id));
        view.folded.retain(|id| project.track(*id).is_ok_and(|track| track.kind == TrackKind::Group));
        view.lanes.retain(|lane| lane_owner(project, &lane.target) == Some(lane.owner));
        if let Some(selection) = &mut view.time_selection {
            selection.tracks.retain(exists);
        }
        // An empty selection still says which lane is being worked in
        // (Ctrl+A selects its points), so it goes only with the lane.
        if view.points.as_ref().is_some_and(|selection| lane_owner(project, &selection.target).is_none()) {
            view.points = None;
        }
        if let Some(selection) = &mut view.points {
            let lane = project.automation.iter().find(|lane| lane.target == selection.target);
            let points = lane.map(|lane| lane.points.as_slice()).unwrap_or_default();
            selection.points.retain(|selected| points.contains(selected));
        }
        if view.renaming_marker.as_ref().is_some_and(|edit| edit.target >= project.markers.len()) {
            view.renaming_marker = None;
        }
    }

    /// Keeps the selected device one of the shown chain: by default its
    /// first, which on a track is the instrument. Cheap, and called every
    /// frame, because selecting another track changes the chain without
    /// the project changing.
    pub fn resolve_device(&mut self, project: &Project) {
        let chain = project.chain(self.shown_chain()).map(Vec::as_slice).unwrap_or_default();
        if !self.selected_device.is_some_and(|id| chain.iter().any(|device| device.id == id)) {
            self.selected_device = chain.first().map(|device| device.id);
        }
    }

    /// Forgets everything tied to the previous project.
    pub fn reset_for_new_project(&mut self) {
        let fresh = Self::new(self.detail_tab);
        let toasts = std::mem::take(&mut self.toasts);
        // The browser is the user's, not the project's: it stays as it is,
        // apart from the devices' search, which narrowed lists for a chain
        // that no longer exists.
        let mut browser = std::mem::replace(&mut self.browser, BrowserState::new());
        browser.search.clear();
        // What was copied in one project can be pasted into the next.
        let clipboard = self.clipboard.take();
        let project_generation = self.project_generation + 1;
        *self = Self { toasts, browser, clipboard, project_generation, ..fresh };
    }
}

/// A clip being renamed in the arrangement. A type of its own, so that the
/// field is told apart from the Clip tab's name field for the same clip.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct InlineClip(pub ClipId);

/// The track (or the master) whose lanes show `target`'s envelope, or
/// `None` when what it automates no longer exists.
pub fn lane_owner(project: &Project, target: &AutomationTarget) -> Option<LaneOwner> {
    match target {
        AutomationTarget::TrackVolume { track } | AutomationTarget::TrackPan { track } => {
            project.track(*track).ok().map(|track| LaneOwner::Track(track.id))
        }
        AutomationTarget::DeviceParam { device, .. } => match project.device_chain(*device).ok()? {
            ChainRef::Track(track) => Some(LaneOwner::Track(track)),
            ChainRef::Master => Some(LaneOwner::Master),
        },
        AutomationTarget::MasterVolume => Some(LaneOwner::Master),
    }
}

/// The audio stream as it is: what was asked of the hardware and what is
/// running. The window's own copy, read again whenever the session says
/// the stream changed, so that nothing on screen has to ask the session.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioSummary {
    /// What was asked for, which is what the settings keep.
    pub config: AudioConfig,
    /// The stream that is running, with the sentence that says why it is
    /// not what was asked for, when it is not.
    pub info: StreamInfo,
    /// How long after the engine renders a frame it is heard, in
    /// milliseconds, as the device reports it. Steadied: the device's
    /// figure moves a little all the time, and a number that flickers in
    /// its last digit says nothing.
    pub latency_ms: f64,
    /// The session's count of changes to the stream when this was read.
    pub revision: u64,
}

impl AudioSummary {
    /// How far the device's figure has to move before the one shown does.
    const LATENCY_STEP_MS: f64 = 0.3;

    pub fn of(session: &Session) -> Self {
        Self {
            config: session.audio_config().clone(),
            info: session.audio_info().clone(),
            latency_ms: session.output_latency_ms(),
            revision: session.audio_revision(),
        }
    }

    /// Takes in the latency the device reports now.
    pub fn hear_latency(&mut self, latency_ms: f64) {
        if (latency_ms - self.latency_ms).abs() >= Self::LATENCY_STEP_MS {
            self.latency_ms = latency_ms;
        }
    }

    /// True when nothing can be heard: a device was wanted and none could
    /// be opened. A run that was asked to go without one is not that.
    pub fn silent(&self) -> bool {
        self.info.device.is_none() && self.info.warning.is_some()
    }
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
    /// The plugins' own windows: opening them, and which are on their way.
    pub editors: &'a mut Editors,
    pub jobs: &'a mut Jobs,
    /// Unsaved work a program that died left behind, and the offer to
    /// bring it back.
    pub recovery: &'a mut Recovery,
    /// Waveforms, previews, the library and drops: everything that reads
    /// audio files, none of it on this thread.
    pub sounds: &'a mut Sounds,
    /// The assistant and its panel down the right side.
    pub assistant: &'a mut Assistant,
    /// The computer keyboard, played as an instrument.
    pub keyboard: &'a mut Keyboard,
    /// The Audio and MIDI panel.
    pub settings: &'a mut Settings,
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
        state.resolve_device(&project);
        assert_eq!((state.shown_chain(), state.selected_device), (ChainRef::Master, Some(limiter)));
        state.select_track(track);
        state.resolve_device(&project);
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
        state.browser.tab = BrowserTab::Sounds;
        state.browser.width = 333.0;
        state.browser.sounds.search = "kick".to_owned();
        state.browser.sounds.form = SoundForm::Loop;
        state.reset_for_new_project();
        assert!(!state.browser.open, "the browser stays as the user left it");
        assert!(state.browser.search.is_empty());
        assert_eq!((state.browser.tab, state.browser.width), (BrowserTab::Sounds, 333.0));
        assert_eq!((state.browser.sounds.search.as_str(), state.browser.sounds.form), ("kick", SoundForm::Loop));
    }

    #[test]
    fn clips_are_added_to_and_taken_out_of_the_selection() {
        let mut state = UiState::new(DetailTab::Clip);
        state.select_clip(TrackId(1), ClipId(10));
        state.toggle_clip(TrackId(2), ClipId(20));
        assert_eq!(state.selected_clips, [ClipId(10), ClipId(20)]);
        assert_eq!(state.selected_clip, Some(ClipId(20)), "the Clip tab shows the one selected last");
        assert_eq!(state.selected_track, Some(TrackId(2)));
        assert!(state.is_clip_selected(ClipId(10)) && !state.is_clip_selected(ClipId(30)));

        state.toggle_clip(TrackId(2), ClipId(20));
        assert_eq!(state.selected_clips, [ClipId(10)]);
        assert_eq!(state.selected_clip, Some(ClipId(10)), "taking the shown clip out shows the one before it");

        state.select_clips(vec![ClipId(30), ClipId(40)]);
        assert_eq!((state.selected_clips.len(), state.selected_clip), (2, Some(ClipId(40))));
        state.select_clip(TrackId(1), ClipId(10));
        assert_eq!(state.selected_clips, [ClipId(10)], "a plain selection replaces the rest");
        state.clear_clip_selection();
        assert!(state.selected_clips.is_empty() && state.selected_clip.is_none());
    }

    #[test]
    fn reconcile_keeps_what_is_left_of_a_multi_selection() {
        let mut project = Project::new("t");
        let track = project.add_track("A");
        let first = project.add_midi_clip(track, "a", 0.0, 4.0).unwrap();
        let second = project.add_midi_clip(track, "b", 4.0, 4.0).unwrap();
        let mut state = UiState::new(DetailTab::Clip);
        state.select_clips(vec![first, second]);

        project.remove_clip(second).unwrap();
        state.reconcile(&project);
        assert_eq!(state.selected_clips, [first]);
        assert_eq!(state.selected_clip, Some(first), "the Clip tab moves to a clip that still exists");
        project.remove_clip(first).unwrap();
        state.reconcile(&project);
        assert!(state.selected_clips.is_empty() && state.selected_clip.is_none());
    }

    #[test]
    fn lanes_folds_and_point_selections_follow_what_they_show() {
        let mut project = Project::new("t");
        let track = project.add_track("A");
        let group = project.add_track_of_kind("G", TrackKind::Group);
        let synth = project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
        let limiter = project.add_device(ChainRef::Master, "builtin:limiter", "Limiter", None).unwrap();
        let cutoff = AutomationTarget::DeviceParam { device: synth, param: "cutoff".into() };
        let ceiling = AutomationTarget::DeviceParam { device: limiter, param: "ceiling".into() };
        let volume = AutomationTarget::TrackVolume { track };
        assert_eq!(lane_owner(&project, &cutoff), Some(LaneOwner::Track(track)));
        assert_eq!(lane_owner(&project, &ceiling), Some(LaneOwner::Master));
        assert_eq!(lane_owner(&project, &AutomationTarget::MasterVolume), Some(LaneOwner::Master));
        assert_eq!(lane_owner(&project, &AutomationTarget::TrackPan { track: TrackId(999) }), None);

        let points = vec![AutomationPoint::new(0.0, 500.0), AutomationPoint::new(4.0, 2000.0)];
        project.set_automation(cutoff.clone(), points.clone()).unwrap();
        let mut state = UiState::new(DetailTab::Clip);
        let view = &mut state.arrangement;
        view.open_lane(LaneOwner::Track(track), cutoff.clone());
        view.open_lane(LaneOwner::Track(track), cutoff.clone());
        view.open_lane(LaneOwner::Track(track), volume.clone());
        view.open_lane(LaneOwner::Master, ceiling.clone());
        assert_eq!(view.lanes.len(), 3, "a lane is opened once");
        view.folded.insert(group);
        view.heights.insert(track, 90.0);
        view.points = Some(PointSelection { target: cutoff.clone(), points: points.clone() });
        view.time_selection = Some(TimeSelection { start: 0.0, end: 4.0, tracks: vec![track, group] });
        assert_eq!(view.height_of(track), 90.0);
        assert_eq!(view.height_of(group), ArrangementState::DEFAULT_TRACK_HEIGHT);

        // One of the two selected points is edited away elsewhere.
        project.set_automation(cutoff.clone(), points[..1].to_vec()).unwrap();
        state.reconcile(&project);
        assert_eq!(state.arrangement.points.as_ref().unwrap().points, points[..1]);

        // The synth goes: its lane and the selection in it go with it.
        project.remove_device(synth).unwrap();
        state.reconcile(&project);
        let targets: Vec<&AutomationTarget> = state.arrangement.lanes.iter().map(|lane| &lane.target).collect();
        assert_eq!(targets, [&volume, &ceiling]);
        assert_eq!(state.arrangement.points, None);

        project.remove_track(track).unwrap();
        project.remove_track(group).unwrap();
        state.reconcile(&project);
        let view = &state.arrangement;
        assert_eq!(view.lanes.len(), 1, "the master's lane stays");
        assert!(view.folded.is_empty() && view.heights.is_empty());
        assert!(view.time_selection.as_ref().unwrap().tracks.is_empty());
    }

    #[test]
    fn closing_a_lane_drops_the_points_selected_in_it() {
        let mut state = UiState::new(DetailTab::Clip);
        let target = AutomationTarget::MasterVolume;
        let view = &mut state.arrangement;
        view.open_lane(LaneOwner::Master, target.clone());
        assert!(view.lane_open(&target));
        view.points = Some(PointSelection { target: target.clone(), points: vec![AutomationPoint::new(0.0, 0.0)] });
        view.close_lane(&target);
        assert!(!view.lane_open(&target) && view.points.is_none());
    }
}
