//! Interaction tests: the whole window is run headlessly against a real
//! session, with pointer and keyboard events fed in as a windowing backend
//! would. They check what the user's gestures do to the project, including
//! that each drag is a single undo step.

use super::app::DawApp;
use super::{DetailTab, UiOptions, arrangement, detail, devices, piano_roll, transport};
use daw_core::device::CompositeFactory;
use daw_core::{ChainRef, ClipId, DeviceId, Note, Project, TrackId};
use daw_dsp::BuiltinFactory;
use daw_engine::{AudioBackend, Session, SharedSession};
use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Pos2, Rect, Vec2, pos2, vec2};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

const SCREEN: Vec2 = vec2(1440.0, 900.0);
const FRAME: f64 = 1.0 / 60.0;

struct Harness {
    ctx: egui::Context,
    app: DawApp,
    session: SharedSession,
    time: f64,
    /// Text and rectangle of every labeled widget on the last frame, as
    /// reported to assistive technology.
    labels: Vec<(String, Rect)>,
}

impl Harness {
    /// Opens the window on a project prepared by `setup`.
    fn new(options: UiOptions, setup: impl FnOnce(&mut Session)) -> Self {
        let factory = Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory)]));
        let mut session = Session::new(factory, AudioBackend::Null { sample_rate: 48_000 }).unwrap();
        setup(&mut session);
        let session: SharedSession = Arc::new(Mutex::new(session));
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let app = DawApp::new(&ctx, session.clone(), options, Arc::default());
        let mut harness = Self { ctx, app, session, time: 0.0, labels: Vec::new() };
        // Widgets are hit-tested against where they were on the previous
        // frame, so the layout has to exist before any input arrives.
        harness.idle(3);
        harness
    }

    /// Runs one frame with the given input events, with `modifiers` held.
    fn step(&mut self, modifiers: Modifiers, mut events: Vec<Event>) {
        self.time += FRAME;
        events.insert(0, Event::ModifiersChanged(modifiers));
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
            time: Some(self.time),
            events,
            focused: true,
            ..Default::default()
        };
        self.app.tick(&self.ctx);
        let mut output = self.ctx.run_ui(input, |ui| self.app.frame(ui));
        // Nothing is drawn, so the font atlas updates have nowhere to go.
        output.textures_delta.clear();
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
            // The update lists nodes in no particular order; reading order
            // makes "the first button labeled X" mean something.
            self.labels.sort_by(|(_, a), (_, b)| a.top().total_cmp(&b.top()).then(a.left().total_cmp(&b.left())));
        }
    }

    /// Center of the `index`th widget, in reading order, whose label is
    /// `label` (or starts with it: menu items append their shortcut).
    fn labeled(&self, label: &str, index: usize) -> Pos2 {
        let exact = self.labels.iter().filter(|(text, _)| text == label);
        let prefixed = self.labels.iter().filter(|(text, _)| text != label && text.starts_with(label));
        match exact.chain(prefixed).nth(index) {
            Some((_, rect)) => rect.center(),
            None => {
                let known: Vec<&str> = self.labels.iter().map(|(text, _)| text.as_str()).collect();
                panic!("nothing on screen is labeled '{label}' (#{index}); labels: {known:?}")
            }
        }
    }

    /// Clicks the first button, menu item or other widget with this label.
    fn click_label(&mut self, label: &str) {
        // Menus and dialogs opened by the previous click take a few frames
        // to appear and settle into their final layout.
        self.idle(3);
        let position = self.labeled(label, 0);
        self.click(position);
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

    // ---- where things are on screen ---------------------------------------

    /// A point in the arrangement lanes: `beat` on track row `row`.
    fn lane(&self, beat: f64, row: usize) -> Pos2 {
        let time = self.app.state().arrangement.time;
        let top = transport::HEIGHT + arrangement::RULER_HEIGHT;
        pos2(time.beat_to_x(arrangement::HEADER_WIDTH, beat), top + (row as f32 + 0.6) * arrangement::TRACK_HEIGHT)
    }

    /// A point on the ruler at `beat`: in the loop brace's strip at the
    /// top, or in the part below it that seeks.
    fn ruler(&self, beat: f64, brace: bool) -> Pos2 {
        let y = transport::HEIGHT + if brace { 6.0 } else { arrangement::RULER_HEIGHT - 6.0 };
        pos2(self.lane(beat, 0).x, y)
    }

    /// A point in the piano roll's note grid.
    fn note_cell(&self, beat: f64, pitch: u8) -> Pos2 {
        let view = &self.app.state().piano;
        let left = piano_roll::SIDEBAR_WIDTH + piano_roll::KEYS_WIDTH;
        let top = SCREEN.y - detail::DEFAULT_HEIGHT + detail::TAB_BAR + piano_roll::RULER_HEIGHT;
        pos2(view.time.beat_to_x(left, beat), view.pitch.pitch_to_y(top, pitch) + view.pitch.row_height / 2.0)
    }
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
    let (mut harness, _, second, _) =
        with_two_tracks(UiOptions { detail_tab: DetailTab::Devices, ..Default::default() });
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
fn the_loop_brace_can_be_moved_stretched_and_toggled() {
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
fn a_track_fader_drag_is_one_undo_step() {
    let (mut harness, first, ..) = with_two_tracks(UiOptions::default());
    let before = harness.undo_depth();
    let row_top = transport::HEIGHT + arrangement::RULER_HEIGHT;
    // The handle sits at unity gain, about 85% along the fader.
    let handle = pos2(
        arrangement::HEADER_PADDING + arrangement::HEADER_FADER_WIDTH * 0.84,
        row_top + arrangement::HEADER_CONTROLS_TOP + 11.0,
    );
    harness.drag(handle, handle - vec2(60.0, 0.0));
    let volume = harness.project().track(first).unwrap().volume_db;
    assert!((-32.0..-18.0).contains(&volume), "dragged well down from 0 dB: {volume}");
    assert_eq!(harness.undo_depth(), before + 1);

    harness.double_click(handle - vec2(60.0, 0.0));
    assert_eq!(harness.project().track(first).unwrap().volume_db, 0.0, "double-click resets to unity");
}

#[test]
fn ctrl_t_adds_a_track_that_can_make_sound() {
    let mut harness = Harness::new(UiOptions::default(), |_| {});
    harness.key(Modifiers::COMMAND, Key::T);
    let project = harness.project();
    assert_eq!(project.tracks.len(), 1);
    assert_eq!(project.tracks[0].devices[0].uri, "builtin:polysynth");
    assert_eq!(harness.app.state().selected_track, Some(project.tracks[0].id));

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
    let under_pointer = after.x_to_beat(arrangement::HEADER_WIDTH, pointer.x);
    assert!((under_pointer - 12.0).abs() < 1e-6, "beat 12 stayed under the pointer, now {under_pointer}");

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

/// A track holding only a reverb, whose first parameters are plain sliders.
fn with_reverb() -> (Harness, DeviceId) {
    let mut device = None;
    let options = UiOptions { detail_tab: DetailTab::Devices, ..Default::default() };
    let harness = Harness::new(options, |session| {
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

    // The first card follows the chain heading; "size" is its first row.
    let tab_top = SCREEN.y - detail::DEFAULT_HEIGHT + detail::TAB_BAR;
    let card_left = devices::STRIP_MARGIN + devices::CHAIN_HEADER_WIDTH + devices::CARD_GAP;
    let bar_left = card_left + devices::CARD_PADDING + devices::LABEL_WIDTH + 8.0;
    let bar_right = card_left + devices::CARD_PADDING + devices::COLUMN_WIDTH - devices::VALUE_WIDTH - 8.0;
    let y = tab_top + devices::STRIP_MARGIN + devices::CARD_HEADER + devices::CARD_PADDING + devices::ROW_HEIGHT / 2.0;
    let at = |fraction: f32| pos2(bar_left + (bar_right - bar_left) * fraction, y);

    let before = harness.undo_depth();
    harness.click(at(0.25));
    assert!((size(&harness) - 0.25).abs() < 0.01, "clicked a quarter of the way along: {}", size(&harness));
    harness.drag(at(0.25), at(0.8));
    assert!((size(&harness) - 0.8).abs() < 0.01, "dragged to 80%: {}", size(&harness));
    assert_eq!(harness.undo_depth(), before + 2, "one step for the click, one for the whole drag");

    // Coming right after other clicks, this arrives as a triple-click.
    harness.double_click(at(0.5));
    assert_eq!(size(&harness), 0.6, "double-click restores the default");
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
    assert_eq!((project.name.as_str(), project.tracks.len()), ("Night Drive", 5));
    let selected = harness.app.state().selected_clip.expect("the demo selects a clip");
    assert_eq!(project.clip(selected).unwrap().name, "Arp A");
}

#[test]
fn a_track_header_mutes_solos_renames_and_deletes() {
    let (mut harness, first, second, _) = with_two_tracks(UiOptions::default());
    let row_top = transport::HEIGHT + arrangement::RULER_HEIGHT;
    let track = |harness: &Harness, id: TrackId| harness.project().track(id).unwrap().clone();

    let chip = |index: f32| pos2(arrangement::HEADER_WIDTH - 62.0 + index * 28.0 + 12.0, row_top + 20.0);
    harness.click(chip(0.0));
    assert!(track(&harness, first).mute && !track(&harness, first).solo);
    harness.click(chip(1.0));
    assert!(track(&harness, first).solo);
    harness.click(chip(0.0));
    assert!(!track(&harness, first).mute, "a second click unmutes");

    // Double-click the name, replace it, confirm with Enter.
    let before = harness.undo_depth();
    harness.double_click(pos2(arrangement::HEADER_PADDING + 30.0, row_top + 20.0));
    harness.idle(2);
    harness.key(Modifiers::COMMAND, Key::A);
    harness.step(Modifiers::NONE, vec![Event::Text("Lead".to_owned())]);
    assert_eq!(track(&harness, first).name, "One", "nothing is edited while typing");
    harness.key(Modifiers::NONE, Key::Enter);
    assert_eq!(track(&harness, first).name, "Lead");
    assert_eq!(harness.undo_depth(), before + 1, "a rename is one undo step");

    // Escape abandons a rename.
    harness.double_click(pos2(arrangement::HEADER_PADDING + 30.0, row_top + 20.0));
    harness.idle(2);
    harness.step(Modifiers::NONE, vec![Event::Text("zzz".to_owned())]);
    harness.key(Modifiers::NONE, Key::Escape);
    assert_eq!(track(&harness, first).name, "Lead");

    // Space typed into a name must not start playback.
    assert!(!harness.session.lock().transport().playing);

    // Right-click the second header and delete the track.
    let header = pos2(arrangement::HEADER_PADDING + 150.0, row_top + arrangement::TRACK_HEIGHT + 20.0);
    harness.click_with(header, PointerButton::Secondary);
    harness.click_label("Delete track");
    let project = harness.project();
    assert_eq!(project.tracks.len(), 1);
    assert!(project.track(second).is_err());
}

#[test]
fn devices_can_be_added_swapped_and_given_presets() {
    let options = UiOptions { detail_tab: DetailTab::Devices, ..Default::default() };
    let mut harness = Harness::new(options, |_| {});
    harness.key(Modifiers::COMMAND, Key::T);
    harness.idle(2);
    let chain = |harness: &Harness| -> Vec<String> {
        harness.project().tracks[0].devices.iter().map(|device| device.uri.clone()).collect()
    };
    assert_eq!(chain(&harness), ["builtin:polysynth"]);

    harness.click_label("+  Add device");
    harness.click_label("Delay");
    assert_eq!(chain(&harness), ["builtin:polysynth", "builtin:delay"], "effects go after the instrument");

    // A second instrument would never receive notes, so it replaces the
    // first, as one undo step.
    harness.click_label("+  Add device");
    harness.click_label("Drum Kit");
    assert_eq!(chain(&harness), ["builtin:drumkit", "builtin:delay"]);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(chain(&harness), ["builtin:polysynth", "builtin:delay"]);

    // The preset menu shows the current preset and applies another.
    assert!(harness.project().tracks[0].devices[0].params.is_empty());
    harness.click_label("init");
    harness.click_label("bass");
    let params = harness.project().tracks[0].devices[0].params.clone();
    assert!(!params.is_empty(), "the preset's values were applied");
    harness.idle(1);
    harness.labeled("bass", 0);
}

#[test]
fn the_master_chain_takes_effects_ahead_of_its_limiter() {
    let options = UiOptions { detail_tab: DetailTab::Devices, ..Default::default() };
    let mut harness = Harness::new(options, |_| {});
    harness.click_label("+  Add device");
    harness.idle(3);
    assert!(harness.labels.iter().any(|(label, _)| label == "Reverb"));
    assert!(harness.labels.iter().all(|(label, _)| label != "Poly Synth"), "no instruments on the master");
    harness.click_label("EQ");
    let master: Vec<String> = harness.project().master.devices.iter().map(|device| device.uri.clone()).collect();
    assert_eq!(master, ["builtin:eq", "builtin:limiter"], "the limiter stays last, guarding the output");
}

#[test]
fn a_device_can_be_bypassed_and_removed_from_its_card() {
    let (mut harness, device) = with_reverb();
    let top = SCREEN.y - detail::DEFAULT_HEIGHT + detail::TAB_BAR + devices::STRIP_MARGIN;
    let card_left = devices::STRIP_MARGIN + devices::CHAIN_HEADER_WIDTH + devices::CARD_GAP;
    // The reverb's eight parameters fit one column.
    let card_right = card_left + devices::COLUMN_WIDTH + devices::CARD_PADDING * 2.0;
    let header_y = top + devices::CARD_HEADER / 2.0;

    harness.click(pos2(card_left + 20.0, header_y));
    assert!(!harness.project().device(device).unwrap().enabled, "the power button bypasses");
    harness.click(pos2(card_left + 20.0, header_y));
    assert!(harness.project().device(device).unwrap().enabled);

    harness.click(pos2(card_right - 18.0, header_y));
    assert!(harness.project().device(device).is_err(), "the close button removes the device");
    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(harness.project().device(device).is_ok());
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
    assert!(harness.labels.iter().all(|(label, _)| label != "Cancel"), "no dialog for an unmodified project");
}

#[test]
fn the_detail_tabs_switch() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    assert_eq!(harness.app.state().detail_tab, DetailTab::Clip);
    let tabs_y = SCREEN.y - detail::DEFAULT_HEIGHT + detail::TAB_BAR / 2.0;
    harness.click(pos2(90.0, tabs_y));
    assert_eq!(harness.app.state().detail_tab, DetailTab::Devices);
    harness.click(pos2(30.0, tabs_y));
    assert_eq!(harness.app.state().detail_tab, DetailTab::Clip);
}

#[test]
fn the_add_track_button_below_the_headers_adds_a_track() {
    let (mut harness, ..) = with_two_tracks(UiOptions::default());
    let below_tracks = transport::HEIGHT + arrangement::RULER_HEIGHT + 2.0 * arrangement::TRACK_HEIGHT;
    harness.click(pos2(arrangement::HEADER_WIDTH / 2.0, below_tracks + 24.0));
    let project = harness.project();
    assert_eq!(project.tracks.len(), 3);
    assert_eq!(project.tracks[2].name, "Track 3");
    assert_eq!(harness.app.state().selected_track, Some(project.tracks[2].id));
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

/// Not a correctness test: prints how long a frame takes with thousands of
/// notes on screen. Run with `cargo test -p daw-app -- --ignored frame_cost`.
#[test]
#[ignore = "a measurement, not an assertion"]
fn frame_cost_with_thousands_of_notes() {
    let mut harness = Harness::new(UiOptions::default(), |session| {
        session
            .edit(|project| {
                for track in 0..8 {
                    let id = project.add_track(format!("Track {track}"));
                    for clip in 0..4 {
                        let clip = project.add_midi_clip(id, "Clip", clip as f64 * 16.0, 16.0)?;
                        let notes: Vec<Note> = (0..250)
                            .map(|n| Note {
                                pitch: 36 + ((n * 7 + track * 5) % 48) as u8,
                                start: n as f64 * 0.0625,
                                length: 0.0625,
                                velocity: 40 + (n % 80) as u8,
                            })
                            .collect();
                        project.clip_mut(clip)?.set_notes(notes)?;
                    }
                }
                Ok(())
            })
            .unwrap();
    });
    // Select a clip so the piano roll is busy too.
    harness.click(harness.lane(4.0, 0));
    harness.idle(5);
    let notes: usize =
        harness.project().tracks.iter().flat_map(|track| &track.clips).map(|clip| clip.notes().len()).sum();

    let frames = 120;
    let started = Instant::now();
    let mut tessellation = Duration::ZERO;
    for _ in 0..frames {
        harness.time += FRAME;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
            time: Some(harness.time),
            focused: true,
            ..Default::default()
        };
        harness.app.tick(&harness.ctx);
        let mut output = harness.ctx.run_ui(input, |ui| harness.app.frame(ui));
        output.textures_delta.clear();
        let before = Instant::now();
        let primitives = harness.ctx.tessellate(output.shapes, output.pixels_per_point);
        tessellation += before.elapsed();
        assert!(!primitives.is_empty());
    }
    let per_frame = started.elapsed() / frames;
    eprintln!(
        "{notes} notes in the project: {:.2} ms per frame, of which {:.2} ms tessellation",
        per_frame.as_secs_f64() * 1000.0,
        (tessellation / frames).as_secs_f64() * 1000.0
    );
}
