//! Interaction tests: the whole window is run headlessly against a real
//! session, with pointer and keyboard events fed in as a windowing backend
//! would. They check what the user's gestures do to the project, including
//! that each drag is a single undo step.
//!
//! This module holds the harness and the tests of the window as a whole;
//! the editing gestures are tested by area in the modules below.

mod assistant;
mod audio;
mod automation;
mod clips;
mod controllers;
mod editors;
mod film;
mod keyboard;
mod keys;
mod making_of;
mod mixer;
mod perf;
mod recording;
mod recovery;
mod ruler;
mod sounds;
mod stage;
mod tracks;

use super::app::TrillApp;
use super::state::{BrowserState, BrowserTab};
use super::{DetailTab, UiOptions, arrangement, browser, detail, devices, piano_roll, status_bar, top_bar};
use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Pos2, Rect, Vec2, pos2, vec2};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::{ChainRef, ClipId, DeviceId, Note, Project, TrackId};
use trill_engine::{Session, SharedSession};
use trill_runtime::{Runtime, RuntimeOptions};

const SCREEN: Vec2 = vec2(1440.0, 900.0);
const FRAME: f64 = 1.0 / 60.0;
/// Height of a Bar and of the gap under it, in the default theme.
const BAR: f32 = 18.0;
const GAP: f32 = 3.0;

struct Harness {
    ctx: egui::Context,
    app: TrillApp,
    /// What the app runs on: no audio device, no plugins, and nothing of
    /// the user's read or written.
    runtime: Arc<Runtime>,
    session: SharedSession,
    time: f64,
    /// Text and rectangle of every labeled widget on the last frame, as
    /// reported to assistive technology.
    labels: Vec<(String, Rect)>,
    /// Label and position (0..=1) of every bar and fader on the last frame.
    values: Vec<(String, f64)>,
    /// Label of every chip and switch on the last frame, and whether it
    /// was on.
    switches: Vec<(String, bool)>,
    /// What the app last put on the system clipboard.
    clipboard: Option<String>,
}

impl Harness {
    /// Opens the window on a project prepared by `setup`.
    fn new(options: UiOptions, setup: impl FnOnce(&mut Session)) -> Self {
        Self::on(Arc::new(Runtime::start(RuntimeOptions::hermetic()).unwrap()), options, setup)
    }

    /// Opens the window on `runtime`: for tests that give it settings to
    /// keep, in a folder of their own.
    fn on(runtime: Arc<Runtime>, options: UiOptions, setup: impl FnOnce(&mut Session)) -> Self {
        setup(&mut runtime.session.lock());
        let session = runtime.session.clone();
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let app = TrillApp::new(&ctx, runtime.clone(), options, Arc::<Mutex<Option<String>>>::default());
        let mut harness = Self {
            ctx,
            app,
            runtime,
            session,
            time: 0.0,
            labels: Vec::new(),
            values: Vec::new(),
            switches: Vec::new(),
            clipboard: None,
        };
        // Widgets are hit-tested against where they were on the previous
        // frame, so the layout has to exist before any input arrives.
        harness.idle(3);
        harness
    }

    /// Runs one frame with the given input events, with `modifiers` held.
    fn step(&mut self, modifiers: Modifiers, events: Vec<Event>) {
        self.step_with_files(modifiers, events, &[], &[]);
    }

    /// Runs one frame in which another program holds `hovered` over the
    /// window, or lets go of `dropped` there.
    fn step_with_files(
        &mut self,
        modifiers: Modifiers,
        mut events: Vec<Event>,
        hovered: &[std::path::PathBuf],
        dropped: &[std::path::PathBuf],
    ) {
        self.time += FRAME;
        events.insert(0, Event::ModifiersChanged(modifiers));
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
            time: Some(self.time),
            events,
            focused: true,
            hovered_files: hovered
                .iter()
                .map(|path| egui::HoveredFile { path: Some(path.clone()), ..Default::default() })
                .collect(),
            dropped_files: dropped.iter().map(|path| Arc::new(sounds::Dropped(path.clone())) as _).collect(),
            ..Default::default()
        };
        self.app.tick(&self.ctx);
        // The window never waits for the session: if something else holds
        // it for a moment (the runtime's housekeeping does, a few times a
        // second), a frame is painted from what the window already has. A
        // test's next click must land on what the session holds now, so
        // here the window is given the time to catch up.
        let mut tries = 0;
        while self.app.snapshot().revision != self.session.lock().revision() && tries < 10_000 {
            std::thread::yield_now();
            self.app.tick(&self.ctx);
            tries += 1;
        }
        let mut output = self.ctx.run_ui(input, |ui| self.app.frame(ui));
        // Nothing is drawn, so the font atlas updates have nowhere to go.
        output.textures_delta.clear();
        for command in output.platform_output.commands.drain(..) {
            if let egui::OutputCommand::CopyText(text) = command {
                self.clipboard = Some(text);
            }
        }
        if let Some(update) = output.platform_output.accesskit_update.take() {
            let labeled = update.nodes.iter().filter_map(|(_, node)| {
                let bounds = node.bounds()?;
                let rect = Rect::from_min_max(
                    pos2(bounds.x0 as f32, bounds.y0 as f32),
                    pos2(bounds.x1 as f32, bounds.y1 as f32),
                );
                // Buttons carry a label; dropdowns their current choice.
                Some((node.label().or(node.value())?.to_owned(), rect))
            });
            self.labels = labeled.collect();
            // Bars and faders, in reading order like the labels.
            let sliders = update.nodes.iter().filter_map(|(_, node)| {
                let bounds = node.bounds()?;
                Some((node.label()?.to_owned(), node.numeric_value()?, (bounds.y0, bounds.x0)))
            });
            let mut sliders: Vec<_> = sliders.collect();
            sliders.sort_by(|(.., a), (.., b)| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
            self.values = sliders.into_iter().map(|(label, value, _)| (label, value)).collect();
            let switches = update.nodes.iter().filter_map(|(_, node)| {
                let on = match node.toggled()? {
                    egui::accesskit::Toggled::True => true,
                    egui::accesskit::Toggled::False => false,
                    egui::accesskit::Toggled::Mixed => return None,
                };
                Some((node.label()?.to_owned(), on))
            });
            self.switches = switches.collect();
            // The update lists nodes in no particular order; reading order
            // makes "the first button labeled X" mean something.
            self.labels.sort_by(|(_, a), (_, b)| a.top().total_cmp(&b.top()).then(a.left().total_cmp(&b.left())));
        }
    }

    /// Rectangles of the widgets whose label is `label` (or starts with it:
    /// menu items append their shortcut), in reading order.
    fn all_labeled(&self, label: &str) -> Vec<Rect> {
        let exact = self.labels.iter().filter(|(text, _)| text == label);
        let prefixed = self.labels.iter().filter(|(text, _)| text != label && text.starts_with(label));
        exact.chain(prefixed).map(|(_, rect)| *rect).collect()
    }

    /// Center of the first widget, in reading order, labeled `label`.
    fn labeled(&self, label: &str) -> Pos2 {
        match self.all_labeled(label).first() {
            Some(rect) => rect.center(),
            None => {
                let known: Vec<&str> = self.labels.iter().map(|(text, _)| text.as_str()).collect();
                panic!("nothing on screen is labeled '{label}'; labels: {known:?}")
            }
        }
    }

    fn is_on_screen(&self, label: &str) -> bool {
        !self.all_labeled(label).is_empty()
    }

    /// Whether the chip or switch labeled `label` is on.
    fn is_on(&self, label: &str) -> bool {
        match self.switches.iter().find(|(text, _)| text == label) {
            Some((_, on)) => *on,
            None => panic!("no chip or switch on screen is labeled '{label}'"),
        }
    }

    /// The position (0..=1) the first bar or fader labeled `label` shows.
    fn shown(&self, label: &str) -> f64 {
        match self.values.iter().find(|(text, _)| text == label) {
            Some((_, value)) => *value,
            None => panic!("no bar or fader on screen is labeled '{label}'"),
        }
    }

    /// Center of the first widget labeled `label` that lies below `y`:
    /// for telling a control in the detail panel from its namesake above.
    fn labeled_below(&self, label: &str, y: f32) -> Pos2 {
        let found = self.all_labeled(label).into_iter().find(|rect| rect.top() >= y);
        found.unwrap_or_else(|| panic!("nothing below {y} is labeled '{label}'")).center()
    }

    /// Clicks the first button, menu item or other widget with this label.
    fn click_label(&mut self, label: &str) {
        // Menus and dialogs opened by the previous click take a few frames
        // to appear and settle into their final layout.
        self.idle(3);
        let position = self.labeled(label);
        self.click(position);
    }

    /// Clicks the last widget, in reading order, with this label: the row
    /// of a menu that opened below a control of the same name.
    fn click_last_label(&mut self, label: &str) {
        self.idle(3);
        let position =
            self.all_labeled(label).last().unwrap_or_else(|| panic!("nothing is labeled '{label}'")).center();
        self.click(position);
    }

    /// Lets enough time pass that the next click is not taken for the
    /// second of a double-click.
    fn settle(&mut self) {
        self.idle(30);
    }

    fn double_click_label(&mut self, label: &str) {
        self.idle(3);
        let position = self.labeled(label);
        self.double_click(position);
    }

    fn idle(&mut self, frames: usize) {
        for _ in 0..frames {
            self.step(Modifiers::NONE, Vec::new());
        }
    }

    fn move_to(&mut self, position: Pos2) {
        self.step(Modifiers::NONE, vec![Event::PointerMoved(position)]);
    }

    fn button(&mut self, position: Pos2, button: PointerButton, pressed: bool) {
        let event = Event::PointerButton { pos: position, button, pressed, modifiers: Modifiers::NONE };
        self.step(Modifiers::NONE, vec![event]);
    }

    fn click_with(&mut self, position: Pos2, button: PointerButton) {
        self.move_to(position);
        self.button(position, button, true);
        self.button(position, button, false);
        self.idle(1);
    }

    fn click(&mut self, position: Pos2) {
        self.click_with(position, PointerButton::Primary);
    }

    fn double_click(&mut self, position: Pos2) {
        self.move_to(position);
        for _ in 0..2 {
            self.button(position, PointerButton::Primary, true);
            self.button(position, PointerButton::Primary, false);
        }
        self.idle(1);
    }

    /// Presses at `from`, moves to `to` over several frames, and releases.
    fn drag(&mut self, from: Pos2, to: Pos2) {
        self.move_to(from);
        self.button(from, PointerButton::Primary, true);
        for step in 1..=8 {
            self.move_to(from + (to - from) * (step as f32 / 8.0));
        }
        self.button(to, PointerButton::Primary, false);
        self.idle(1);
    }

    fn key(&mut self, modifiers: Modifiers, key: Key) {
        for pressed in [true, false] {
            let event = Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
            self.step(modifiers, vec![event]);
        }
        self.idle(1);
    }

    fn type_text(&mut self, text: &str) {
        self.step(Modifiers::NONE, vec![Event::Text(text.to_owned())]);
    }

    fn project(&self) -> Project {
        self.session.lock().project().clone()
    }

    /// Number of undo steps available.
    fn undo_depth(&self) -> usize {
        let mut session = self.session.lock();
        let mut depth = 0;
        while session.undo() {
            depth += 1;
        }
        for _ in 0..depth {
            session.redo();
        }
        depth
    }

    /// Waits for the audio thread to act on a transport command.
    fn wait_for(&self, what: &str, condition: impl Fn(&Session) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !condition(&self.session.lock()) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Runs frames until the window is as `condition` says: for what a
    /// helper thread has to finish first.
    fn wait_until(&mut self, what: &str, condition: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(120);
        while !condition(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
            self.idle(1);
        }
    }

    // ---- where things are on screen ---------------------------------------

    /// Left edge of everything right of the browser.
    fn work_left(&self) -> f32 {
        if self.app.state().browser.open { self.app.state().browser.width } else { 0.0 }
    }

    /// Left edge of the arrangement's lanes.
    fn lanes_left(&self) -> f32 {
        self.work_left() + arrangement::HEADER_WIDTH
    }

    /// Top of the `row`th track.
    fn row_top(&self, row: usize) -> f32 {
        top_bar::HEIGHT + arrangement::RULER_HEIGHT + row as f32 * arrangement::TRACK_HEIGHT
    }

    /// A point in the arrangement lanes: `beat` on track row `row`.
    fn lane(&self, beat: f64, row: usize) -> Pos2 {
        let time = self.app.state().arrangement.time;
        pos2(time.beat_to_x(self.lanes_left(), beat), self.row_top(row) + 0.6 * arrangement::TRACK_HEIGHT)
    }

    /// A point on the ruler at `beat`: on the loop bar along its top, or in
    /// the part below it that seeks.
    fn ruler(&self, beat: f64, loop_bar: bool) -> Pos2 {
        let y = top_bar::HEIGHT + if loop_bar { 3.0 } else { arrangement::RULER_HEIGHT - 5.0 };
        pos2(self.lane(beat, 0).x, y)
    }

    /// A point in the header of track row `row`: `x` from the header's
    /// left edge, `y` from its top.
    fn header(&self, row: usize, x: f32, y: f32) -> Pos2 {
        pos2(self.work_left() + x, self.row_top(row) + y)
    }

    /// A point on the volume bar of track row `row`, `fraction` along it.
    fn volume_bar(&self, row: usize, fraction: f32) -> Pos2 {
        let left = arrangement::HEADER_LEFT + arrangement::HEADER_INDEX;
        let right = arrangement::HEADER_WIDTH - arrangement::HEADER_RIGHT - arrangement::HEADER_PAN_WIDTH - GAP;
        self.header(row, left + (right - left) * fraction, arrangement::HEADER_CONTROLS_TOP + BAR / 2.0)
    }

    /// Center of the mute (0), solo (1) or arm (2) chip of track row `row`.
    fn header_chip(&self, row: usize, index: f32) -> Pos2 {
        let right = arrangement::HEADER_WIDTH - arrangement::HEADER_RIGHT;
        let chip = arrangement::HEADER_CHIP;
        let left = right - (3.0 - index) * chip.x - (2.0 - index) * GAP;
        self.header(row, left + chip.x / 2.0, arrangement::HEADER_NAME_TOP + chip.y / 2.0)
    }

    /// Top of the detail panel at its default height.
    fn detail_top(&self) -> f32 {
        SCREEN.y - status_bar::HEIGHT - detail::DEFAULT_HEIGHT
    }

    /// Top of the master row.
    fn master_top(&self) -> f32 {
        self.detail_top() - arrangement::MASTER_HEIGHT
    }

    /// A point in the piano roll's note grid.
    fn note_cell(&self, beat: f64, pitch: u8) -> Pos2 {
        let view = &self.app.state().piano;
        let left = self.work_left() + piano_roll::SIDEBAR_WIDTH + piano_roll::KEYS_WIDTH;
        let top = self.detail_top() + detail::TAB_BAR + piano_roll::RULER_HEIGHT;
        pos2(view.time.beat_to_x(left, beat), view.pitch.pitch_to_y(top, pitch) + view.pitch.row_height / 2.0)
    }

    /// A point in the velocity lane: at `beat`, `up` pixels above its floor.
    fn velocity_lane(&self, beat: f64, up: f32) -> Pos2 {
        pos2(self.note_cell(beat, 60).x, SCREEN.y - status_bar::HEIGHT - up)
    }

    /// The first panel of the device rack.
    fn first_panel(&self) -> Pos2 {
        pos2(self.work_left() + devices::RACK_PADDING, self.detail_top() + detail::TAB_BAR + devices::RACK_PADDING)
    }

    /// A point on the `row`th bar of the first panel's first section,
    /// `fraction` along a column `width` wide.
    fn first_panel_bar(&self, row: usize, width: f32, fraction: f32) -> Pos2 {
        let top = devices::PANEL_HEADER + devices::BODY_TOP + devices::SECTION_HEADER + 2.0 * GAP;
        self.first_panel() + vec2(devices::BODY_PADDING + width * fraction, top + row as f32 * (BAR + GAP) + BAR / 2.0)
    }
}

/// What the newer tests add to the harness: modifiers on pointer gestures,
/// the clipboard, the wheel, and where the arrangement's newer parts are.
impl Harness {
    fn button_holding(&mut self, modifiers: Modifiers, position: Pos2, button: PointerButton, pressed: bool) {
        let event = Event::PointerButton { pos: position, button, pressed, modifiers };
        self.step(modifiers, vec![event]);
    }

    /// A click with `modifiers` held.
    fn click_holding(&mut self, modifiers: Modifiers, position: Pos2) {
        self.step(modifiers, vec![Event::PointerMoved(position)]);
        self.button_holding(modifiers, position, PointerButton::Primary, true);
        self.button_holding(modifiers, position, PointerButton::Primary, false);
        self.idle(1);
    }

    fn right_click(&mut self, position: Pos2) {
        self.click_with(position, PointerButton::Secondary);
    }

    /// Presses at `from` and moves to `to` over several frames with
    /// `modifiers` held, leaving the button down.
    fn drag_start(&mut self, modifiers: Modifiers, from: Pos2, to: Pos2) {
        self.step(modifiers, vec![Event::PointerMoved(from)]);
        self.button_holding(modifiers, from, PointerButton::Primary, true);
        for step in 1..=8 {
            self.step(modifiers, vec![Event::PointerMoved(from + (to - from) * (step as f32 / 8.0))]);
        }
    }

    /// Lets go of a drag begun with [`Self::drag_start`].
    fn drag_end(&mut self, position: Pos2) {
        self.button(position, PointerButton::Primary, false);
        self.idle(1);
    }

    /// A whole drag with `modifiers` held.
    fn drag_holding(&mut self, modifiers: Modifiers, from: Pos2, to: Pos2) {
        self.drag_start(modifiers, from, to);
        self.button_holding(modifiers, to, PointerButton::Primary, false);
        self.idle(1);
    }

    /// Turns the wheel `lines` over `position` with `modifiers` held, and
    /// lets the smooth scrolling run out.
    fn wheel(&mut self, modifiers: Modifiers, position: Pos2, lines: f32) {
        self.step(modifiers, vec![Event::PointerMoved(position)]);
        let wheel = Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, lines),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers,
        };
        self.step(modifiers, vec![wheel]);
        for _ in 0..20 {
            self.step(modifiers, Vec::new());
        }
        self.idle(1);
    }

    /// Ctrl+C as a windowing backend delivers it: a clipboard event, not a
    /// key press.
    fn copy(&mut self) {
        self.step(Modifiers::COMMAND, vec![Event::Copy]);
        self.idle(1);
    }

    fn cut(&mut self) {
        self.step(Modifiers::COMMAND, vec![Event::Cut]);
        self.idle(1);
    }

    /// Ctrl+V: the backend hands over whatever text the system clipboard
    /// holds, which is what the app last put there.
    fn paste(&mut self) {
        let text = self.clipboard.clone().expect("something was copied");
        self.step(Modifiers::COMMAND, vec![Event::Paste(text)]);
        self.idle(1);
    }

    /// Screen y of a height in the arrangement's track list.
    fn content_y(&self, y: f32) -> f32 {
        top_bar::HEIGHT + arrangement::RULER_HEIGHT + y
    }

    /// A point at `beat`, `y` down the arrangement's track list: for rows
    /// that are not all one height, such as automation lanes.
    fn lanes_at(&self, beat: f64, y: f32) -> Pos2 {
        pos2(self.lane(beat, 0).x, self.content_y(y))
    }

    /// A point in the ruler's marker strip at `beat`.
    fn marker_strip(&self, beat: f64) -> Pos2 {
        pos2(self.lane(beat, 0).x, top_bar::HEIGHT + arrangement::LOOP_STRIP + arrangement::MARKER_STRIP / 2.0)
    }

    /// The clips of the track at `index`, as (start, length) pairs.
    fn spans(&self, index: usize) -> Vec<(f64, f64)> {
        self.project().tracks[index].clips.iter().map(|clip| (clip.start, clip.length)).collect()
    }

    fn selected_clips(&self) -> Vec<ClipId> {
        self.app.state().selected_clips.clone()
    }

    fn hint(&self) -> String {
        self.app.state().hint.text().to_owned()
    }
}

/// Three tracks. The first has clips at bars 3 and 5 (beats 8 and 16), each
/// a bar long with a note on its second beat; the second has one at bar 3.
fn three_tracks(session: &mut Session) -> ([TrackId; 3], [ClipId; 3]) {
    session
        .edit(|project| {
            let tracks = [project.add_track("One"), project.add_track("Two"), project.add_track("Three")];
            let note = [Note { pitch: 60, start: 1.0, length: 1.0, velocity: 100 }];
            let mut clips = Vec::new();
            for (track, name, start) in [(tracks[0], "A", 8.0), (tracks[0], "B", 16.0), (tracks[1], "C", 8.0)] {
                let clip = project.add_midi_clip(track, name, start, 4.0)?;
                project.clip_mut(clip)?.add_notes(&note)?;
                clips.push(clip);
            }
            Ok((tracks, [clips[0], clips[1], clips[2]]))
        })
        .unwrap()
}

fn with_three_tracks(options: UiOptions) -> (Harness, [TrackId; 3], [ClipId; 3]) {
    let mut ids = None;
    let harness = Harness::new(options, |session| ids = Some(three_tracks(session)));
    let (tracks, clips) = ids.unwrap();
    (harness, tracks, clips)
}

/// Two tracks; the first has a one-bar clip at bar 3 holding one note.
fn two_tracks(session: &mut Session) -> (TrackId, TrackId, ClipId) {
    session
        .edit(|project| {
            let first = project.add_track("One");
            let second = project.add_track("Two");
            let clip = project.add_midi_clip(first, "Clip", 8.0, 4.0)?;
            project.clip_mut(clip)?.add_notes(&[Note { pitch: 60, start: 1.0, length: 1.0, velocity: 100 }])?;
            Ok((first, second, clip))
        })
        .unwrap()
}

fn with_two_tracks(options: UiOptions) -> (Harness, TrackId, TrackId, ClipId) {
    let mut ids = None;
    let harness = Harness::new(options, |session| ids = Some(two_tracks(session)));
    let (first, second, clip) = ids.unwrap();
    (harness, first, second, clip)
}

fn devices_tab() -> UiOptions {
    UiOptions { detail_tab: DetailTab::Devices, ..Default::default() }
}

fn chain_uris(project: &Project, chain: ChainRef) -> Vec<String> {
    project.chain(chain).unwrap().iter().map(|device| device.uri.clone()).collect()
}

#[test]
fn edits_made_elsewhere_appear_without_any_input() {
    let mut harness = Harness::new(UiOptions::default(), |_| {});
    assert!(harness.app.snapshot().project.tracks.is_empty());

    // What an MCP server does: edit the shared session from outside.
    let track = harness.session.lock().edit(|project| Ok(project.add_track("From elsewhere"))).unwrap();
    harness.idle(1);
    assert_eq!(harness.app.snapshot().project.tracks[0].name, "From elsewhere");
    assert_eq!(harness.app.state().selected_track, Some(track), "the first track is selected by default");

    harness.session.lock().undo();
    harness.idle(1);
    assert!(harness.app.snapshot().project.tracks.is_empty());
    assert_eq!(harness.app.state().selected_track, None, "a selection that no longer exists is dropped");
}

#[test]
fn clicking_selects_clips_and_tracks() {
    let (mut harness, first, second, clip) = with_two_tracks(UiOptions::default());
    harness.click(harness.lane(10.0, 0));
    assert_eq!(harness.app.state().selected_clip, Some(clip));
    assert_eq!(harness.app.state().selected_track, Some(first));

    harness.click(harness.lane(10.0, 1));
    assert_eq!(harness.app.state().selected_clip, None, "clicking empty space deselects");
    assert_eq!(harness.app.state().selected_track, Some(second));
    assert_eq!(harness.undo_depth(), 1, "selecting edits nothing");
}

#[test]
fn dragging_a_clip_moves_it_across_time_and_tracks_as_one_undo_step() {
    let (mut harness, first, second, clip) = with_two_tracks(UiOptions::default());
    let before = harness.undo_depth();

    // Grab the clip a beat into its body; drop it a bar later on track two.
    harness.drag(harness.lane(9.0, 0), harness.lane(13.0, 1));
    let project = harness.project();
    assert_eq!(project.clip_track(clip).unwrap(), second);
    assert_eq!(project.clip(clip).unwrap().start, 12.0, "moved four beats, snapped to the grid");
    assert_eq!(project.clip(clip).unwrap().length, 4.0);
    assert_eq!(harness.app.state().selected_clip, Some(clip));
    assert_eq!(harness.app.state().selected_track, Some(second), "the selection follows the clip to its new track");
    assert_eq!(harness.undo_depth(), before + 1, "the whole drag is one undo step");

    harness.key(Modifiers::COMMAND, Key::Z);
    let project = harness.project();
    assert_eq!(project.clip_track(clip).unwrap(), first);
    assert_eq!(project.clip(clip).unwrap().start, 8.0);

    harness.key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    assert_eq!(harness.project().clip(clip).unwrap().start, 12.0, "redo");
    harness.key(Modifiers::COMMAND, Key::Z);
    harness.key(Modifiers::COMMAND, Key::Y);
    assert_eq!(harness.project().clip(clip).unwrap().start, 12.0, "Ctrl+Y also redoes");
}

#[test]
fn dragging_a_clips_right_edge_resizes_it() {
    let (mut harness, _, _, clip) = with_two_tracks(UiOptions::default());
    let before = harness.undo_depth();
    let edge = harness.lane(12.0, 0) - vec2(3.0, 0.0);
    harness.drag(edge, harness.lane(16.0, 0));
    let project = harness.project();
    assert_eq!(project.clip(clip).unwrap().start, 8.0);
    assert_eq!(project.clip(clip).unwrap().length, 8.0);
    assert_eq!(harness.undo_depth(), before + 1);

    // It cannot be dragged shut.
    let edge = harness.lane(16.0, 0) - vec2(3.0, 0.0);
    harness.drag(edge, harness.lane(2.0, 0));
    assert!(harness.project().clip(clip).unwrap().length > 0.0);
}

#[test]
fn double_clicking_an_empty_lane_creates_a_one_bar_clip() {
    let (mut harness, _, second, _) = with_two_tracks(devices_tab());
    harness.double_click(harness.lane(21.5, 1));
    let project = harness.project();
    let created = &project.track(second).unwrap().clips[0];
    assert_eq!((created.start, created.length), (20.0, 4.0), "the bar under the pointer");
    assert!(created.midi().is_some());
    assert_eq!(harness.app.state().selected_clip, Some(created.id));
    assert_eq!(harness.app.state().detail_tab, DetailTab::Clip, "the new clip opens in the piano roll");
}

#[test]
fn delete_and_duplicate_act_on_the_selected_clip() {
    let (mut harness, first, _, clip) = with_two_tracks(UiOptions::default());
    harness.click(harness.lane(10.0, 0));
    harness.key(Modifiers::COMMAND, Key::D);
    let project = harness.project();
    let clips = &project.track(first).unwrap().clips;
    assert_eq!(clips.len(), 2);
    assert_eq!((clips[1].start, clips[1].notes().len()), (12.0, 1), "the copy follows the original");
    assert_eq!(harness.app.state().selected_clip, Some(clips[1].id), "and becomes the selection");

    harness.key(Modifiers::NONE, Key::Delete);
    let project = harness.project();
    assert_eq!(project.track(first).unwrap().clips.len(), 1);
    assert_eq!(project.track(first).unwrap().clips[0].id, clip);
    assert_eq!(harness.app.state().selected_clip, None);

    // With nothing selected, Delete does nothing.
    harness.key(Modifiers::NONE, Key::Delete);
    assert_eq!(harness.project().track(first).unwrap().clips.len(), 1);
}

#[test]
fn the_ruler_seeks_and_space_plays() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    harness.click(harness.ruler(8.0, false));
    harness.wait_for("the seek", |session| (session.transport().position_beats - 8.0).abs() < 1e-9);

    harness.key(Modifiers::NONE, Key::Space);
    harness.wait_for("playback to start", |session| session.transport().playing);
    harness.wait_for("the song to advance", |session| session.transport().position_beats > 8.0);
    // The app has to notice that playback started before Space can stop it.
    harness.idle(1);
    harness.key(Modifiers::NONE, Key::Space);
    harness.wait_for("playback to stop", |session| !session.transport().playing);

    harness.key(Modifiers::NONE, Key::Home);
    harness.wait_for("the rewind", |session| session.transport().position_beats == 0.0);
}

#[test]
fn the_transport_buttons_play_stop_and_loop() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    harness.click_label("Play");
    harness.wait_for("playback to start", |session| session.transport().playing);
    harness.idle(1);
    harness.click_label("Stop");
    harness.wait_for("playback to stop", |session| !session.transport().playing);

    assert!(!harness.project().loop_region.enabled);
    harness.click_label("Loop");
    assert!(harness.project().loop_region.enabled);
}

#[test]
fn the_loop_bar_can_be_moved_stretched_and_toggled() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    assert_eq!(harness.project().loop_region.start, 0.0);
    assert_eq!(harness.project().loop_region.end, 16.0);
    let before = harness.undo_depth();

    harness.drag(harness.ruler(6.0, true), harness.ruler(10.0, true));
    let region = harness.project().loop_region;
    assert_eq!((region.start, region.end), (4.0, 20.0), "moved a bar, keeping its length");

    harness.drag(harness.ruler(20.0, true) - vec2(2.0, 0.0), harness.ruler(12.0, true));
    let region = harness.project().loop_region;
    assert_eq!((region.start, region.end), (4.0, 12.0), "its end was dragged in");
    assert_eq!(harness.undo_depth(), before + 2, "one undo step per drag");

    assert!(!region.enabled);
    harness.key(Modifiers::NONE, Key::L);
    assert!(harness.project().loop_region.enabled);
}

#[test]
fn a_track_volume_drag_is_one_undo_step() {
    let (mut harness, first, ..) = with_two_tracks(UiOptions::default());
    let before = harness.undo_depth();
    // The pointer is the value: pressing 40% along the bar sets the volume
    // there, and dragging carries it along.
    harness.drag(harness.volume_bar(0, 0.84), harness.volume_bar(0, 0.4));
    let volume = harness.project().track(first).unwrap().volume_db;
    assert!((-23.0..-21.0).contains(&volume), "40% of the way up the fader scale: {volume}");
    assert_eq!(harness.undo_depth(), before + 1, "press and drag are one step");

    harness.double_click(harness.volume_bar(0, 0.4));
    assert_eq!(harness.project().track(first).unwrap().volume_db, 0.0, "double-click resets to unity");
}

#[test]
fn the_pan_cell_pans_and_recenters() {
    let (mut harness, first, ..) = with_two_tracks(UiOptions::default());
    let pan_left = arrangement::HEADER_WIDTH - arrangement::HEADER_RIGHT - arrangement::HEADER_PAN_WIDTH;
    let at = |harness: &Harness, fraction: f32| {
        let x = pan_left + arrangement::HEADER_PAN_WIDTH * fraction;
        harness.header(0, x, arrangement::HEADER_CONTROLS_TOP + BAR / 2.0)
    };
    harness.click(at(&harness, 0.75));
    let pan = harness.project().track(first).unwrap().pan;
    assert!((pan - 0.5).abs() < 0.05, "three quarters across is half right: {pan}");
    harness.double_click(at(&harness, 0.2));
    assert_eq!(harness.project().track(first).unwrap().pan, 0.0, "double-click centers");
}

#[test]
fn ctrl_t_adds_a_track_that_can_make_sound() {
    let mut harness = Harness::new(UiOptions::default(), |_| {});
    harness.key(Modifiers::COMMAND, Key::T);
    let project = harness.project();
    assert_eq!(project.tracks.len(), 1);
    assert_eq!(project.tracks[0].devices[0].uri, "builtin:polysynth");
    assert_eq!(project.tracks[0].color, 0xA87FD1, "new tracks take the palette's hues, violet first");
    assert_eq!(harness.app.state().selected_track, Some(project.tracks[0].id));

    harness.key(Modifiers::COMMAND, Key::T);
    assert_eq!(harness.project().tracks[1].color, 0x72A9D9, "then sky");
    harness.key(Modifiers::COMMAND, Key::Z);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(harness.project().tracks.is_empty(), "track and instrument are undone together");
}

#[test]
fn ctrl_scroll_zooms_the_timeline_around_the_pointer() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    let pointer = harness.lane(12.0, 0);
    harness.move_to(pointer);
    let before = harness.app.state().arrangement.time;

    let wheel = Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: vec2(0.0, 3.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: Modifiers::COMMAND,
    };
    harness.step(Modifiers::COMMAND, vec![wheel]);
    harness.idle(1);
    let after = harness.app.state().arrangement.time;
    assert!(after.pixels_per_beat > before.pixels_per_beat, "zoomed in");
    // Beat 12, to within the precision of a screen coordinate.
    let was_under_pointer = before.x_to_beat(harness.lanes_left(), pointer.x);
    let under_pointer = after.x_to_beat(harness.lanes_left(), pointer.x);
    assert!((was_under_pointer - 12.0).abs() < 1e-4);
    assert!((under_pointer - was_under_pointer).abs() < 1e-9, "the beat under the pointer stayed: {under_pointer}");

    // Without Ctrl the wheel pans instead.
    let wheel = Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: vec2(0.0, -3.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: Modifiers::NONE,
    };
    harness.step(Modifiers::NONE, vec![wheel]);
    harness.idle(20);
    let panned = harness.app.state().arrangement.time;
    assert_eq!(panned.pixels_per_beat, after.pixels_per_beat);
    assert!(panned.origin > after.origin, "scrolled later in time");
}

#[test]
fn the_piano_roll_adds_moves_and_deletes_notes() {
    let (mut harness, _, _, clip) = with_two_tracks(UiOptions::default());
    harness.click(harness.lane(10.0, 0));
    harness.idle(2);
    let notes = |harness: &Harness| harness.project().clip(clip).unwrap().notes().to_vec();
    assert_eq!(notes(&harness).len(), 1);
    let before = harness.undo_depth();

    // A click on empty grid adds a sixteenth at the grid cell under it.
    harness.click(harness.note_cell(2.3, 64));
    let added = Note { pitch: 64, start: 2.25, length: 0.25, velocity: 100 };
    assert_eq!(notes(&harness), [Note { pitch: 60, start: 1.0, length: 1.0, velocity: 100 }, added]);
    assert_eq!(harness.app.state().selected_notes, [added]);

    // Drag the existing note up a fourth and half a beat later.
    harness.drag(harness.note_cell(1.3, 60), harness.note_cell(1.8, 65));
    let moved = Note { pitch: 65, start: 1.5, length: 1.0, velocity: 100 };
    assert_eq!(notes(&harness), [moved, added]);
    assert_eq!(harness.app.state().selected_notes, [moved]);
    assert_eq!(harness.undo_depth(), before + 2, "one step for the click, one for the drag");

    // Its right edge stretches it.
    harness.drag(harness.note_cell(2.5, 65) - vec2(2.0, 0.0), harness.note_cell(3.0, 65));
    assert_eq!(notes(&harness)[0], Note { length: 1.5, ..moved });

    // Right-click deletes; Delete removes the selection.
    harness.click_with(harness.note_cell(2.3, 64), PointerButton::Secondary);
    assert_eq!(notes(&harness).len(), 1);
    harness.click(harness.note_cell(1.6, 65));
    harness.key(Modifiers::NONE, Key::Delete);
    assert!(notes(&harness).is_empty());
    assert_eq!(
        harness.project().track_index(harness.project().clip_track(clip).unwrap()).unwrap(),
        0,
        "the clip stays"
    );
}

#[test]
fn arrow_keys_nudge_the_selected_notes() {
    let (mut harness, _, _, clip) = with_two_tracks(UiOptions::default());
    harness.click(harness.lane(10.0, 0));
    harness.idle(2);
    harness.click(harness.note_cell(1.5, 60));
    harness.key(Modifiers::NONE, Key::ArrowUp);
    harness.key(Modifiers::NONE, Key::ArrowRight);
    let note = harness.project().clip(clip).unwrap().notes()[0];
    assert_eq!((note.pitch, note.start), (61, 1.25), "up a semitone, right one sixteenth");
}

#[test]
fn dragging_a_velocity_stem_changes_the_note_as_one_undo_step() {
    let (mut harness, _, _, clip) = with_two_tracks(UiOptions::default());
    harness
        .session
        .lock()
        .edit(|project| {
            let more = [Note { pitch: 64, start: 2.0, length: 1.0, velocity: 80 }];
            project.clip_mut(clip)?.add_notes(&more)
        })
        .unwrap();
    harness.click(harness.lane(10.0, 0));
    harness.idle(2);
    let velocities = |harness: &Harness| -> Vec<u8> {
        harness.project().clip(clip).unwrap().notes().iter().map(|note| note.velocity).collect()
    };
    assert_eq!(velocities(&harness), [100, 80]);
    let before = harness.undo_depth();

    // The stem of the first note, dragged from near its top to near the
    // floor of the lane.
    harness.drag(harness.velocity_lane(1.0, 30.0), harness.velocity_lane(1.0, 10.0));
    let after = velocities(&harness);
    assert!(after[0] < 40, "dragged down: {after:?}");
    assert_eq!(after[1], 80, "the other note is not selected, so it stays");
    assert_eq!(harness.undo_depth(), before + 1, "the whole drag is one undo step");
    assert_eq!(harness.app.state().selected_notes.len(), 1, "the dragged note becomes the selection");

    // With both selected, the second follows the first by the same amount.
    harness.key(Modifiers::COMMAND, Key::A);
    assert_eq!(harness.app.state().selected_notes.len(), 2);
    let first = after[0];
    harness.drag(harness.velocity_lane(1.0, 10.0), harness.velocity_lane(1.0, 20.0));
    let raised = velocities(&harness);
    assert!(raised[0] > first);
    assert_eq!(raised[1] as i32 - 80, raised[0] as i32 - first as i32, "{raised:?}");
}

#[test]
fn the_clip_sidebar_sets_grid_velocity_and_quantizes() {
    let (mut harness, _, _, clip) = with_two_tracks(UiOptions::default());
    harness
        .session
        .lock()
        .edit(|project| {
            project.clip_mut(clip)?.set_notes(vec![Note { pitch: 60, start: 1.1, length: 1.0, velocity: 100 }])
        })
        .unwrap();
    harness.click(harness.lane(10.0, 0));
    harness.idle(2);

    harness.click_label("1/8");
    assert_eq!(harness.app.state().piano.snap.label(), "1/8");
    harness.click_label("Quantize");
    assert_eq!(harness.project().clip(clip).unwrap().notes()[0].start, 1.0, "onto the eighth-note grid");

    harness.click_label("Mute");
    assert!(harness.project().clip(clip).unwrap().muted, "the clip, not its track");
    assert!(!harness.project().tracks[0].mute);
    // The transport has a Loop button too; this is the clip's.
    harness.click(harness.labeled_below("Loop", harness.detail_top()));
    assert!(!harness.project().loop_region.enabled);
    assert_eq!(harness.project().clip(clip).unwrap().midi().unwrap().loop_length, Some(4.0));

    // With a note selected, the velocity bar sets its velocity.
    harness.click(harness.note_cell(1.5, 60));
    harness.idle(1);
    let bar = harness.labeled("Velocity");
    harness.click(bar);
    let velocity = harness.project().clip(clip).unwrap().notes()[0].velocity;
    assert!((60..=68).contains(&velocity), "the middle of the bar is the middle of the range: {velocity}");
}

/// A track holding only a reverb, whose first parameters are plain bars.
fn with_reverb() -> (Harness, DeviceId) {
    let mut device = None;
    let harness = Harness::new(devices_tab(), |session| {
        let track = session.edit(|project| Ok(project.add_track("FX"))).unwrap();
        device = Some(session.add_device(ChainRef::Track(track), "builtin:reverb", None, None).unwrap());
    });
    (harness, device.unwrap())
}

#[test]
fn a_parameter_bar_sets_values_by_click_and_by_drag() {
    let (mut harness, device) = with_reverb();
    let size = |harness: &Harness| {
        let params = harness.session.lock().device_params(device).unwrap();
        params.iter().find(|param| param.info.id == "size").unwrap().value
    };
    assert_eq!(size(&harness), 0.6);

    // "Size" is the first bar of the reverb's first section.
    let at = |harness: &Harness, fraction: f32| harness.first_panel_bar(0, 150.0, fraction);
    let before = harness.undo_depth();
    harness.click(at(&harness, 0.25));
    assert!((size(&harness) - 0.25).abs() < 0.01, "clicked a quarter of the way along: {}", size(&harness));
    harness.drag(at(&harness, 0.25), at(&harness, 0.8));
    assert!((size(&harness) - 0.8).abs() < 0.01, "dragged to 80%: {}", size(&harness));
    assert_eq!(harness.undo_depth(), before + 2, "one step for the click, one for the whole drag");

    // Coming right after other clicks, this arrives as a triple-click.
    harness.double_click(at(&harness, 0.5));
    assert_eq!(size(&harness), 0.6, "double-click restores the default");
}

#[test]
fn a_compressor_is_given_a_track_to_listen_to_from_its_panel() {
    let (mut device, mut kick) = (None, None);
    let mut harness = Harness::new(devices_tab(), |session| {
        let tracks = session.edit(|project| Ok((project.add_track("Bass"), project.add_track("Kick")))).unwrap();
        device = Some(session.add_device(ChainRef::Track(tracks.0), "builtin:compressor", None, None).unwrap());
        session.add_device(ChainRef::Track(tracks.0), "builtin:reverb", None, None).unwrap();
        kick = Some(tracks.1);
    });
    let (device, kick) = (device.unwrap(), kick.unwrap());
    let listens_to = |harness: &Harness| harness.project().device(device).unwrap().sidechain;
    let detector = |harness: &Harness| {
        let params = harness.session.lock().device_params(device).unwrap();
        params.iter().find(|param| param.info.id == "detector").unwrap().value
    };

    // The compressor says what it listens to; the reverb beside it, which
    // listens to nothing but its input, has no such row.
    assert_eq!(harness.all_labeled("Listen to").len(), 1);
    let before = harness.undo_depth();
    harness.click_label("Listen to: Own input");
    harness.click_last_label("Kick");
    assert_eq!(listens_to(&harness), Some(kick));
    assert_eq!(harness.undo_depth(), before + 1, "one step");
    harness.idle(3);
    assert!(harness.is_on_screen("Listen to: Kick"), "and the panel says which track");

    // Back to what comes into it.
    harness.click_label("Listen to: Kick");
    harness.click_last_label("Own input");
    assert_eq!(listens_to(&harness), None);

    // The detector is a pair of chips beside it.
    assert_eq!(detector(&harness), 0.0);
    harness.settle();
    harness.click_label("Bus");
    assert_eq!(detector(&harness), 1.0);
}

#[test]
fn an_empty_project_offers_ways_to_start() {
    let mut harness = Harness::new(UiOptions::default(), |_| {});
    harness.click_label("Add first track");
    assert_eq!(harness.project().tracks.len(), 1);
    assert_eq!(harness.project().tracks[0].devices[0].uri, "builtin:polysynth");

    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(harness.project().tracks.is_empty());
    harness.click_label("Open demo song");
    let project = harness.project();
    assert_eq!((project.name.as_str(), project.tracks.len()), ("Night Drive", 7));
    let selected = harness.app.state().selected_clip.expect("the demo selects a clip");
    assert_eq!(project.clip(selected).unwrap().name, "Arp A");
}

#[test]
fn a_track_header_mutes_solos_renames_and_deletes() {
    let (mut harness, first, second, _) = with_two_tracks(UiOptions::default());
    let track = |harness: &Harness, id: TrackId| harness.project().track(id).unwrap().clone();

    harness.click(harness.header_chip(0, 0.0));
    assert!(track(&harness, first).mute && !track(&harness, first).solo);
    harness.click(harness.header_chip(0, 1.0));
    assert!(track(&harness, first).solo);
    harness.click(harness.header_chip(0, 0.0));
    assert!(!track(&harness, first).mute, "a second click unmutes");

    // Double-click the name, replace it, confirm with Enter.
    let before = harness.undo_depth();
    let name = harness.header(0, 60.0, 14.0);
    harness.double_click(name);
    harness.idle(2);
    harness.key(Modifiers::COMMAND, Key::A);
    harness.type_text("Lead");
    assert_eq!(track(&harness, first).name, "One", "nothing is edited while typing");
    harness.key(Modifiers::NONE, Key::Enter);
    assert_eq!(track(&harness, first).name, "Lead");
    assert_eq!(harness.undo_depth(), before + 1, "a rename is one undo step");

    // Escape abandons a rename.
    harness.double_click(name);
    harness.idle(2);
    harness.type_text("zzz");
    harness.key(Modifiers::NONE, Key::Escape);
    assert_eq!(track(&harness, first).name, "Lead");

    // Space typed into a name must not start playback.
    assert!(!harness.session.lock().transport().playing);

    // Right-click the second header and delete the track.
    harness.click_with(harness.header(1, 120.0, 14.0), PointerButton::Secondary);
    harness.click_label("Delete track");
    let project = harness.project();
    assert_eq!(project.tracks.len(), 1);
    assert!(project.track(second).is_err());
}

#[test]
fn the_browser_adds_and_swaps_devices_and_applies_presets() {
    let mut harness = Harness::new(devices_tab(), |_| {});
    harness.key(Modifiers::COMMAND, Key::T);
    harness.idle(2);
    let chain = |harness: &Harness| chain_uris(&harness.project(), ChainRef::Track(harness.project().tracks[0].id));
    assert_eq!(chain(&harness), ["builtin:polysynth"]);

    // A single click only points at a device; a double-click adds it.
    harness.click_label("Delay");
    assert_eq!(chain(&harness), ["builtin:polysynth"]);
    harness.idle(30);
    harness.double_click_label("Delay");
    assert_eq!(chain(&harness), ["builtin:polysynth", "builtin:delay"], "effects go after the instrument");

    // Three quick clicks are one double-click, not two.
    let reverb = harness.labeled("Reverb");
    harness.double_click(reverb);
    harness.click(reverb);
    assert_eq!(chain(&harness).len(), 3, "one reverb: {:?}", chain(&harness));
    harness.key(Modifiers::COMMAND, Key::Z);

    // A second instrument would never receive notes, so it replaces the
    // first, as one undo step.
    harness.double_click_label("Drum Kit");
    assert_eq!(chain(&harness), ["builtin:drumkit", "builtin:delay"]);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(chain(&harness), ["builtin:polysynth", "builtin:delay"]);

    // The browser lists the presets of the selected device, the synth, and
    // applies one on a click.
    assert!(harness.project().tracks[0].devices[0].params.is_empty());
    harness.click_label("bass");
    let params = harness.project().tracks[0].devices[0].params.clone();
    assert!(!params.is_empty(), "the preset's values were applied");
    harness.idle(2);
    assert_eq!(harness.all_labeled("bass").len(), 2, "the browser's row and the chip in the device's header");

    // Selecting another device lists its presets instead (the delay has
    // none).
    let delay = harness.project().tracks[0].devices[1].id;
    // The synth's panel is 832 wide; the delay's follows after the gap.
    let delay_name = harness.first_panel() + vec2(832.0 + devices::PANEL_GAP + 60.0, devices::PANEL_HEADER / 2.0);
    harness.click(delay_name);
    assert_eq!(harness.app.state().selected_device, Some(delay));
    harness.idle(2);
    assert!(!harness.is_on_screen("warm_pad"));
}

#[test]
fn a_devices_preset_chip_opens_its_presets() {
    let mut harness = Harness::new(devices_tab(), |_| {});
    harness.key(Modifiers::COMMAND, Key::T);
    harness.idle(2);
    // The chip in the synth's header shows the current preset; the browser
    // row of the same name comes first in reading order.
    let chips = harness.all_labeled("init");
    assert_eq!(chips.len(), 2);
    harness.click(chips[1].center());
    harness.idle(3);
    // The popup's rows are to the right of the browser's.
    let rows = harness.all_labeled("supersaw");
    let in_popup = rows.iter().find(|rect| rect.left() > harness.work_left()).expect("the popup lists presets");
    harness.click(in_popup.center());
    let device = &harness.project().tracks[0].devices[0];
    assert_eq!(device.params.get("unison"), Some(&7.0), "supersaw stacks seven voices");
}

#[test]
fn a_devices_sound_is_saved_from_its_preset_menu_and_given_back() {
    let mut harness = Harness::new(devices_tab(), |_| {});
    harness.key(Modifiers::COMMAND, Key::T);
    harness.idle(2);
    let synth = |harness: &Harness| harness.project().tracks[0].devices[0].clone();
    // Found before the session is taken: `synth` takes it too.
    let device = synth(&harness).id;
    harness.session.lock().set_device_params(device, &[("cutoff".into(), 432.0)]).unwrap();
    harness.idle(2);

    // The menu in the device's header offers to save the sound; its name
    // is typed where the device's name is.
    harness.click_last_label("Preset");
    harness.click_label("Save preset\u{2026}");
    harness.idle(2);
    assert!(harness.app.state().naming_preset.is_some(), "the name is being typed");
    harness.type_text("Dark Pad");
    harness.key(Modifiers::NONE, Key::Enter);
    harness.idle(3);
    assert!(harness.app.state().naming_preset.is_none());
    let saved = harness.session.lock().saved_presets().list("builtin:polysynth");
    assert_eq!(saved.iter().map(|preset| preset.name.as_str()).collect::<Vec<_>>(), ["Dark Pad"]);
    assert_eq!(synth(&harness).preset.map(|preset| preset.name), Some("Dark Pad".to_string()));
    // The header says which preset the device is on, and the browser
    // lists it with the synth's own.
    assert_eq!(harness.all_labeled("Dark Pad").len(), 2, "the browser's row and the menu in the header");

    // Another preset, then the saved one again from the browser: the
    // sound comes back whole.
    harness.click_label("bass");
    assert_ne!(synth(&harness).params.get("cutoff"), Some(&432.0));
    harness.idle(2);
    assert_eq!(harness.all_labeled("Dark Pad").len(), 1, "the header shows the preset the device is on now");
    harness.click_label("Dark Pad");
    assert_eq!(synth(&harness).params.get("cutoff"), Some(&432.0));

    // Escape leaves nothing saved, and the name it offers is the one the
    // device is on.
    harness.idle(2);
    harness.click_last_label("Dark Pad");
    harness.click_label("Save preset\u{2026}");
    harness.idle(2);
    assert_eq!(harness.app.state().naming_preset.as_ref().map(|edit| edit.text.as_str()), Some("Dark Pad"));
    harness.key(Modifiers::NONE, Key::Escape);
    harness.idle(2);
    assert!(harness.app.state().naming_preset.is_none());
    assert_eq!(harness.session.lock().saved_presets().len(), 1);
}

#[test]
fn the_browser_search_narrows_its_lists_and_ctrl_b_hides_it() {
    let (mut harness, ..) = with_two_tracks(devices_tab());
    assert!(harness.is_on_screen("Reverb") && harness.is_on_screen("Chorus"));
    harness.click(pos2(harness.work_left() / 2.0, top_bar::HEIGHT + browser::HEADER + 8.0 + 11.0));
    harness.type_text("rev");
    harness.idle(2);
    assert!(harness.is_on_screen("Reverb"));
    assert!(!harness.is_on_screen("Chorus") && !harness.is_on_screen("Poly Synth"));

    // Typing in the search field is not a shortcut.
    harness.type_text(" ");
    assert!(!harness.session.lock().transport().playing);
    harness.key(Modifiers::NONE, Key::Escape);

    let lanes_before = harness.lanes_left();
    harness.key(Modifiers::COMMAND, Key::B);
    assert!(!harness.app.state().browser.open);
    assert_eq!(harness.lanes_left(), lanes_before - BrowserState::DEFAULT_WIDTH, "the arrangement takes the room");
    assert!(!harness.is_on_screen("Reverb"));
    harness.click_label("View");
    harness.click_label("Browser");
    assert!(harness.app.state().browser.open);
}

#[test]
fn the_master_row_selects_the_master_and_takes_effects_ahead_of_its_limiter() {
    let (mut harness, first, ..) = with_two_tracks(devices_tab());
    assert_eq!(harness.app.state().shown_chain(), ChainRef::Track(first));

    harness.click(pos2(harness.work_left() + 60.0, harness.master_top() + arrangement::MASTER_HEIGHT / 2.0));
    assert_eq!(harness.app.state().shown_chain(), ChainRef::Master);
    harness.double_click_label("EQ");
    let project = harness.project();
    assert_eq!(
        chain_uris(&project, ChainRef::Master),
        ["builtin:eq", "builtin:limiter"],
        "the limiter stays last, guarding the output"
    );
    assert!(project.track(first).unwrap().devices.is_empty(), "the track was not touched");
    harness.idle(2);
    assert!(harness.is_on_screen("EQ on the master"), "the master row lists its devices");

    // The master carries no notes: an instrument gets a track of its own.
    harness.double_click_label("Drum Kit");
    let project = harness.project();
    assert_eq!(project.tracks.len(), 3);
    assert_eq!(project.tracks[2].devices[0].uri, "builtin:drumkit");
    assert_eq!(chain_uris(&project, ChainRef::Master).len(), 2);

    // Clicking a track goes back to its chain.
    harness.click(harness.header(0, 120.0, 14.0));
    assert_eq!(harness.app.state().shown_chain(), ChainRef::Track(first));
    harness.click_label("Limiter on the master");
    assert_eq!(harness.app.state().shown_chain(), ChainRef::Master);
    assert_eq!(harness.app.state().detail_tab, DetailTab::Devices);
}

#[test]
fn the_master_volume_bar_is_one_undo_step() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    let before = harness.undo_depth();
    let right = harness.work_left() + arrangement::HEADER_WIDTH - arrangement::HEADER_RIGHT;
    let y = harness.master_top() + arrangement::MASTER_HEIGHT / 2.0;
    let at = |fraction: f32| pos2(right - arrangement::MASTER_VOLUME_WIDTH * (1.0 - fraction), y);
    harness.drag(at(0.8), at(0.5));
    let volume = harness.project().master.volume_db;
    assert!((-17.5..-15.0).contains(&volume), "halfway up the fader scale: {volume}");
    assert_eq!(harness.undo_depth(), before + 1);
}

#[test]
fn a_device_can_be_bypassed_and_removed_from_its_panel() {
    let (mut harness, device) = with_reverb();
    let panel = harness.first_panel();
    let header_y = panel.y + devices::PANEL_HEADER / 2.0;
    // The reverb's panel: two columns and the gap between them.
    let panel_right = panel.x + 2.0 * 150.0 + devices::COLUMN_GAP + 2.0 * devices::BODY_PADDING;

    harness.click(pos2(panel.x + 10.0, header_y));
    assert!(!harness.project().device(device).unwrap().enabled, "the dot bypasses");
    harness.click(pos2(panel.x + 10.0, header_y));
    assert!(harness.project().device(device).unwrap().enabled);

    harness.click(pos2(panel_right - 12.0, header_y));
    assert!(harness.project().device(device).is_err(), "the cross removes the device");
    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(harness.project().device(device).is_ok());
}

#[test]
fn chips_set_choices_and_switches() {
    let mut synth = None;
    let mut harness = Harness::new(devices_tab(), |session| {
        let track = session.edit(|project| Ok(project.add_track("Lead"))).unwrap();
        synth = Some(session.add_device(ChainRef::Track(track), "builtin:polysynth", None, None).unwrap());
    });
    let value = |harness: &Harness, id: &str| {
        let params = harness.session.lock().device_params(synth.unwrap()).unwrap();
        params.iter().find(|param| param.info.id == id).unwrap().value
    };
    assert_eq!(value(&harness, "osc1_wave"), 2.0, "a saw by default");
    // The first row of the first section is oscillator one's wave.
    let sin = harness.all_labeled("Sin");
    assert_eq!(sin.len(), 2, "one chip row per oscillator");
    harness.click(sin[0].center());
    assert_eq!(value(&harness, "osc1_wave"), 0.0);
    assert_eq!(value(&harness, "osc2_wave"), 2.0, "the second oscillator has its own row");

    assert_eq!(value(&harness, "mono"), 0.0);
    harness.click_label("Mono");
    assert_eq!(value(&harness, "mono"), 1.0);
    harness.click_label("Poly");
    assert_eq!(value(&harness, "mono"), 0.0);

    harness.click_label("HP");
    assert_eq!(value(&harness, "filter_type"), 2.0);
}

#[test]
fn starting_a_new_project_asks_before_discarding_changes() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    harness.click_label("File");
    harness.click_label("New project");
    harness.click_label("Cancel");
    assert_eq!(harness.project().tracks.len(), 2, "cancelled: nothing is lost");

    harness.key(Modifiers::COMMAND, Key::N);
    harness.click_label("Discard and start new");
    let project = harness.project();
    assert!(project.tracks.is_empty());
    assert_eq!(project.name, "Untitled");
    assert_eq!(harness.app.state().selected_track, None);

    // With nothing to lose there is no question to answer.
    harness.key(Modifiers::COMMAND, Key::N);
    harness.idle(3);
    assert!(!harness.is_on_screen("Cancel"), "no dialog for an unmodified project");
}

#[test]
fn the_edit_menu_undoes_and_redoes() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    harness.click_label("Edit");
    harness.click_label("Add track");
    assert_eq!(harness.project().tracks.len(), 3);
    harness.click_label("Edit");
    harness.click_label("Undo");
    assert_eq!(harness.project().tracks.len(), 2);
    harness.click_label("Edit");
    harness.click_label("Redo");
    assert_eq!(harness.project().tracks.len(), 3);
}

#[test]
fn the_detail_tabs_switch() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    assert_eq!(harness.app.state().detail_tab, DetailTab::Clip);
    // The browser has a tab called Devices too; the detail panel's is the
    // lower of the two.
    harness.click_last_label("Devices");
    assert_eq!(harness.app.state().detail_tab, DetailTab::Devices);
    assert_eq!(harness.app.state().browser.tab, BrowserTab::Devices);
    harness.click_label("Clip");
    assert_eq!(harness.app.state().detail_tab, DetailTab::Clip);
}

#[test]
fn the_corner_button_adds_a_track() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    harness.click_label("New track");
    let project = harness.project();
    assert_eq!(project.tracks.len(), 3);
    assert_eq!(project.tracks[2].name, "Track 3");
    assert_eq!(harness.app.state().selected_track, Some(project.tracks[2].id));
}

#[test]
fn the_tempo_can_be_dragged_and_typed() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    assert_eq!(harness.project().tempo, 120.0);
    let before = harness.undo_depth();
    let tempo = harness.labeled("Tempo");
    harness.drag(tempo, tempo - vec2(0.0, 40.0));
    assert_eq!(harness.project().tempo, 130.0, "a quarter of a BPM per pixel, upward");
    assert_eq!(harness.undo_depth(), before + 1);

    harness.double_click(tempo);
    harness.idle(2);
    harness.key(Modifiers::COMMAND, Key::A);
    harness.type_text("96");
    harness.key(Modifiers::NONE, Key::Enter);
    assert_eq!(harness.project().tempo, 96.0);

    // Escape leaves the tempo alone.
    harness.double_click(tempo);
    harness.idle(2);
    harness.type_text("7");
    harness.key(Modifiers::NONE, Key::Escape);
    assert_eq!(harness.project().tempo, 96.0);
}

#[test]
fn the_status_bar_explains_what_is_under_the_pointer() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    assert_eq!(harness.app.state().hint.text(), "", "nothing is hovered yet");
    let play = harness.labeled("Play");
    harness.move_to(play);
    harness.idle(1);
    assert!(harness.app.state().hint.text().starts_with("Play"), "{:?}", harness.app.state().hint.text());
    harness.move_to(harness.volume_bar(0, 0.5));
    harness.idle(1);
    assert!(harness.app.state().hint.text().starts_with("Volume"), "{:?}", harness.app.state().hint.text());
    harness.move_to(pos2(SCREEN.x - 300.0, SCREEN.y - 10.0));
    harness.idle(2);
    assert_eq!(harness.app.state().hint.text(), "", "over the status bar itself there is nothing to explain");
}

#[test]
fn the_plugin_scan_is_reported_until_the_catalog_arrives() {
    let mut harness = Harness::new(UiOptions::default(), |_| {});
    // The catalog is read on a helper thread; with only built-in devices it
    // arrives almost at once.
    let deadline = Instant::now() + Duration::from_secs(3);
    while !harness.app.catalog_ready() {
        assert!(Instant::now() < deadline, "the device catalog never arrived");
        std::thread::sleep(Duration::from_millis(5));
        harness.idle(1);
    }
    assert!(harness.is_on_screen("Poly Synth") && harness.is_on_screen("Utility"));
}

#[test]
fn the_view_menu_switches_themes() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    assert_eq!(harness.app.theme().name, "noir");
    harness.click_label("View");
    harness.click_label("Graphite");
    harness.idle(1);
    assert_eq!(harness.app.theme().name, "graphite");
    assert!(!harness.ctx.global_style().visuals.dark_mode);
    harness.click_label("View");
    harness.click_label("Noir");
    harness.idle(1);
    assert_eq!(harness.app.theme().name, "noir");

    let graphite = Harness::new(UiOptions { theme: "graphite", ..Default::default() }, |_| {});
    assert_eq!(graphite.app.theme().name, "graphite", "the command line can start in another theme");
}

#[test]
fn piano_keys_audition_while_held() {
    let (mut harness, first, ..) = with_two_tracks(UiOptions::default());
    harness.click(harness.lane(10.0, 0));
    harness.idle(2);
    let held = |harness: &Harness| harness.app.state().audition.map(|audition| (audition.track, audition.pitch));

    // Just left of the note grid is the keyboard.
    let key = |pitch: u8| harness.note_cell(0.0, pitch) - vec2(piano_roll::KEYS_WIDTH / 2.0, 0.0);
    let (c4, e4) = (key(60), key(64));
    harness.move_to(c4);
    harness.button(c4, PointerButton::Primary, true);
    harness.idle(1);
    assert_eq!(held(&harness), Some((first, 60)));
    // Sliding to another key plays that one instead.
    harness.move_to(e4);
    harness.idle(1);
    assert_eq!(held(&harness), Some((first, 64)));
    harness.button(e4, PointerButton::Primary, false);
    harness.idle(2);
    assert_eq!(held(&harness), None, "releasing the mouse releases the note");
    assert_eq!(harness.undo_depth(), 1, "auditioning edits nothing");
}
