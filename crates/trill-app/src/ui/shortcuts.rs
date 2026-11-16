//! Keyboard shortcuts: one table, read by the key handling, by the menus
//! (for the keys they show beside a command) and by the reference window.
//! A shortcut cannot be documented without working, or work without being
//! documented, because all three look here.

use super::state::Cx;
use eframe::egui::{self, Align2, Event, Key, KeyboardShortcut, Modifiers, Rect, Sense, pos2, vec2};
use trill_noir::{paint, text};

/// Something the keyboard can ask for. What a command does may depend on
/// the pane the user is working in: Delete removes clips in the arrangement
/// and notes in the piano roll.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Command {
    New,
    Open,
    Save,
    SaveAs,
    ExportWav,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Duplicate,
    Delete,
    SelectAll,
    /// Drops the selection, or leaves the tool that is active.
    Escape,
    /// Move the selection by grid steps (first) and rows or semitones.
    Nudge(i32, i32),
    Split,
    MuteClips,
    RenameClip,
    LoopSelection,
    DeleteTime,
    DuplicateTime,
    Quantize,
    AddMarker,
    AddTrack,
    GroupTrack,
    ToggleAutomation,
    TogglePlay,
    Rewind,
    ToggleLoop,
    ToggleMetronome,
    ToggleFollow,
    ZoomToSelection,
    ZoomToSong,
    ToggleDraw,
    ToggleBlade,
    ToggleBrowser,
    ShowShortcuts,
}

/// When a binding is live.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum When {
    /// Even while a text field has the keyboard.
    Always,
    /// Unless something is being typed.
    NotTyping,
    /// Only while the pane being worked in has a selection; otherwise the
    /// key keeps its usual meaning (arrows scroll lists).
    WithSelection,
}

/// The heading a binding is listed under in the reference.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Section {
    File,
    Edit,
    Arrangement,
    Tracks,
    Transport,
    View,
}

impl Section {
    const ALL: [Section; 6] =
        [Section::File, Section::Edit, Section::Arrangement, Section::Tracks, Section::Transport, Section::View];

    fn title(self) -> &'static str {
        match self {
            Section::File => "File",
            Section::Edit => "Edit",
            Section::Arrangement => "Arrangement",
            Section::Tracks => "Tracks",
            Section::Transport => "Transport",
            Section::View => "View",
        }
    }
}

pub struct Binding {
    pub command: Command,
    pub modifiers: Modifiers,
    pub key: Key,
    pub when: When,
    pub section: Section,
    /// What it does, as the reference words it.
    pub does: &'static str,
}

const CTRL: Modifiers = Modifiers::COMMAND;
const SHIFT: Modifiers = Modifiers::SHIFT;
const CTRL_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
const NONE: Modifiers = Modifiers::NONE;

const fn bind(
    command: Command,
    modifiers: Modifiers,
    key: Key,
    when: When,
    section: Section,
    does: &'static str,
) -> Binding {
    Binding { command, modifiers, key, when, section, does }
}

/// Every keyboard shortcut. A command bound to several keys is listed once
/// per key, with the same description.
pub const BINDINGS: &[Binding] = &[
    bind(Command::New, CTRL, Key::N, When::Always, Section::File, "New project"),
    bind(Command::Open, CTRL, Key::O, When::Always, Section::File, "Open a project"),
    bind(Command::Save, CTRL, Key::S, When::Always, Section::File, "Save"),
    bind(Command::SaveAs, CTRL_SHIFT, Key::S, When::Always, Section::File, "Save as"),
    bind(Command::ExportWav, CTRL_SHIFT, Key::E, When::Always, Section::File, "Export the song as WAV"),
    bind(Command::Undo, CTRL, Key::Z, When::NotTyping, Section::Edit, "Undo"),
    bind(Command::Redo, CTRL_SHIFT, Key::Z, When::NotTyping, Section::Edit, "Redo"),
    bind(Command::Redo, CTRL, Key::Y, When::NotTyping, Section::Edit, "Redo"),
    bind(Command::Cut, CTRL, Key::X, When::NotTyping, Section::Edit, "Cut the selection"),
    bind(Command::Copy, CTRL, Key::C, When::NotTyping, Section::Edit, "Copy the selection"),
    bind(Command::Paste, CTRL, Key::V, When::NotTyping, Section::Edit, "Paste at the edit cursor"),
    bind(Command::Duplicate, CTRL, Key::D, When::NotTyping, Section::Edit, "Duplicate the selection after itself"),
    bind(Command::Delete, NONE, Key::Delete, When::NotTyping, Section::Edit, "Delete the selection"),
    bind(Command::Delete, NONE, Key::Backspace, When::NotTyping, Section::Edit, "Delete the selection"),
    bind(Command::SelectAll, CTRL, Key::A, When::NotTyping, Section::Edit, "Select everything in the pane"),
    bind(Command::Escape, NONE, Key::Escape, When::NotTyping, Section::Edit, "Deselect, or put the tool away"),
    bind(Command::Nudge(-1, 0), NONE, Key::ArrowLeft, When::WithSelection, Section::Edit, "Nudge by one grid step"),
    bind(Command::Nudge(1, 0), NONE, Key::ArrowRight, When::WithSelection, Section::Edit, "Nudge by one grid step"),
    bind(Command::Nudge(0, 1), NONE, Key::ArrowUp, When::WithSelection, Section::Edit, "Notes: by a semitone"),
    bind(Command::Nudge(0, -1), NONE, Key::ArrowDown, When::WithSelection, Section::Edit, "Notes: by a semitone"),
    bind(Command::Nudge(0, 12), SHIFT, Key::ArrowUp, When::WithSelection, Section::Edit, "Notes: by an octave"),
    bind(Command::Nudge(0, -12), SHIFT, Key::ArrowDown, When::WithSelection, Section::Edit, "Notes: by an octave"),
    bind(Command::Quantize, CTRL, Key::U, When::NotTyping, Section::Edit, "Notes: quantize to the grid"),
    bind(Command::Split, CTRL, Key::E, When::NotTyping, Section::Arrangement, "Split clips at the edit cursor"),
    bind(
        Command::MuteClips,
        NONE,
        Key::Num0,
        When::NotTyping,
        Section::Arrangement,
        "Mute or unmute the selected clips",
    ),
    bind(Command::RenameClip, CTRL, Key::R, When::NotTyping, Section::Arrangement, "Rename the selected clip"),
    bind(Command::LoopSelection, CTRL, Key::L, When::NotTyping, Section::Arrangement, "Loop the time selection"),
    bind(
        Command::DeleteTime,
        CTRL_SHIFT,
        Key::Delete,
        When::NotTyping,
        Section::Arrangement,
        "Delete the selected time",
    ),
    bind(
        Command::DuplicateTime,
        CTRL_SHIFT,
        Key::D,
        When::NotTyping,
        Section::Arrangement,
        "Duplicate the selected time",
    ),
    bind(Command::AddMarker, NONE, Key::M, When::NotTyping, Section::Arrangement, "Add a marker at the edit cursor"),
    bind(Command::ToggleDraw, NONE, Key::B, When::NotTyping, Section::Arrangement, "Pencil: draw automation in steps"),
    bind(Command::ToggleBlade, NONE, Key::C, When::NotTyping, Section::Arrangement, "Blade: click a clip to split it"),
    bind(Command::AddTrack, CTRL, Key::T, When::NotTyping, Section::Tracks, "Add a track"),
    bind(Command::GroupTrack, CTRL, Key::G, When::NotTyping, Section::Tracks, "Put the selected track in a new group"),
    bind(
        Command::ToggleAutomation,
        NONE,
        Key::A,
        When::NotTyping,
        Section::Tracks,
        "Show or hide the selected track\u{2019}s automation",
    ),
    bind(Command::TogglePlay, NONE, Key::Space, When::NotTyping, Section::Transport, "Play or stop"),
    bind(Command::Rewind, NONE, Key::Home, When::NotTyping, Section::Transport, "Return to the start"),
    bind(Command::ToggleLoop, NONE, Key::L, When::NotTyping, Section::Transport, "Loop on or off"),
    bind(Command::ToggleMetronome, NONE, Key::K, When::NotTyping, Section::Transport, "Metronome on or off"),
    bind(Command::ToggleFollow, NONE, Key::F, When::NotTyping, Section::Transport, "Follow the playhead"),
    bind(Command::ZoomToSelection, NONE, Key::Z, When::NotTyping, Section::View, "Zoom to the selection"),
    bind(Command::ZoomToSong, SHIFT, Key::Z, When::NotTyping, Section::View, "Zoom to the whole song"),
    bind(Command::ToggleBrowser, CTRL, Key::B, When::NotTyping, Section::View, "Show or hide the browser"),
    bind(Command::ShowShortcuts, NONE, Key::F1, When::Always, Section::View, "This reference"),
];

/// What the mouse does, for the reference. Nothing reads these but the
/// reference window; the gestures themselves live in the views.
const GESTURES: &[(&str, &[(&str, &str)])] = &[
    (
        "Clips",
        &[
            ("Drag", "Move, also across tracks"),
            ("Ctrl-drag", "Copy"),
            ("Drag an edge", "Trim"),
            ("Drag a top corner", "Fade (audio clips)"),
            ("Shift- or Ctrl-click", "Add to the selection"),
            ("Drag in empty space", "Select time and clips"),
            ("Double-click", "Open in the piano roll"),
            ("Double-click the name", "Rename"),
            ("Alt while dragging", "No snapping"),
        ],
    ),
    (
        "Automation",
        &[
            ("Click", "Add a point"),
            ("Drag a point", "Move it; Shift for fine values"),
            ("Double-click a point", "Remove it"),
            ("Drag a line", "Move both of its ends"),
            ("Alt-drag a line", "Bend it"),
            ("Drag in empty space", "Select points"),
        ],
    ),
    (
        "Sounds",
        &[
            ("Click a sound", "Hear it; Esc stops it"),
            ("Arrows", "Move through the list"),
            ("Enter or Space", "Play it again"),
            ("Type", "Search"),
            ("Drag onto a track", "An audio clip where it lands"),
            ("Drag below the tracks", "A new audio track"),
            ("Drag onto a pad or a sampler", "It plays that sound"),
            ("Double-click", "Add at the edit cursor"),
        ],
    ),
    (
        "Ruler and view",
        &[
            ("Click the ruler", "Move the edit cursor"),
            ("Drag the ruler", "Select time"),
            ("Drag the top strip", "Draw the loop"),
            ("Double-click the marker strip", "Add a marker"),
            ("Scroll", "Along the timeline"),
            ("Ctrl+scroll", "Zoom the timeline"),
            ("Ctrl+Alt+scroll", "Zoom track height"),
            ("Alt+scroll", "Through the tracks"),
        ],
    ),
];

/// The keys of a binding as a menu shows them: `"Ctrl+Shift+Z"`.
fn chord_label(modifiers: Modifiers, key: Key) -> String {
    let mut label = String::new();
    if modifiers.command || modifiers.ctrl {
        label.push_str("Ctrl+");
    }
    if modifiers.alt {
        label.push_str("Alt+");
    }
    if modifiers.shift {
        label.push_str("Shift+");
    }
    label.push_str(match key {
        Key::Delete => "Del",
        Key::Escape => "Esc",
        other => other.name(),
    });
    label
}

/// The keys that run `command`, for the menu item that does the same: the
/// first binding in the table. Empty if the command has no shortcut.
pub fn keys(command: Command) -> String {
    BINDINGS
        .iter()
        .find(|binding| binding.command == command)
        .map(|binding| chord_label(binding.modifiers, binding.key))
        .unwrap_or_default()
}

/// How many modifier keys a chord holds.
fn specificity(modifiers: Modifiers) -> usize {
    [modifiers.alt, modifiers.shift, modifiers.command || modifiers.ctrl].into_iter().filter(|held| *held).count()
}

/// The command a windowing backend's clipboard event stands for. Backends
/// turn Ctrl+C, Ctrl+X and Ctrl+V into these events instead of key presses,
/// so the bindings of those chords have to answer to them as well.
fn clipboard_chord(event: &Event) -> Option<Key> {
    match event {
        Event::Copy => Some(Key::C),
        Event::Cut => Some(Key::X),
        Event::Paste(_) => Some(Key::V),
        _ => None,
    }
}

/// Collects the commands for the keys pressed this frame. `typing` is true
/// while a text field has the keyboard; `selection` while the pane being
/// worked in has something selected.
pub fn read(ctx: &egui::Context, typing: bool, selection: bool) -> Vec<Command> {
    let live = |binding: &&Binding| match binding.when {
        When::Always => true,
        When::NotTyping => !typing,
        When::WithSelection => !typing && selection,
    };
    // A chord also matches a press with more modifiers held, so the more
    // specific chords have to be asked first.
    let mut bindings: Vec<&Binding> = BINDINGS.iter().filter(live).collect();
    bindings.sort_by_key(|binding| std::cmp::Reverse(specificity(binding.modifiers)));

    let mut commands = Vec::new();
    ctx.input_mut(|input| {
        for binding in &bindings {
            if input.consume_shortcut(&KeyboardShortcut::new(binding.modifiers, binding.key)) {
                commands.push(binding.command);
            }
        }
        if !typing {
            for key in input.events.iter().filter_map(clipboard_chord) {
                let bound = bindings.iter().find(|binding| binding.key == key && binding.modifiers == CTRL);
                commands.extend(bound.map(|binding| binding.command));
            }
        }
    });
    commands
}

// ---- the reference window ---------------------------------------------------

const COLUMN: f32 = 290.0;
const COLUMN_GAP: f32 = 22.0;
const ROW: f32 = 19.0;
const HEADING: f32 = 24.0;

/// One line of the reference: what happens, and the keys or gesture for it.
struct Line {
    does: String,
    keys: String,
}

/// The table as the reference lists it: per section, one line per command,
/// with every chord bound to it.
fn reference() -> Vec<(&'static str, Vec<Line>)> {
    let mut sections = Vec::new();
    for section in Section::ALL {
        let mut lines: Vec<Line> = Vec::new();
        for binding in BINDINGS.iter().filter(|binding| binding.section == section) {
            let keys = chord_label(binding.modifiers, binding.key);
            match lines.iter_mut().find(|line| line.does == binding.does) {
                Some(line) => line.keys = format!("{} / {keys}", line.keys),
                None => lines.push(Line { does: binding.does.to_owned(), keys }),
            }
        }
        sections.push((section.title(), lines));
    }
    for (title, gestures) in GESTURES {
        let lines = gestures.iter().map(|(keys, does)| Line { does: (*does).to_owned(), keys: (*keys).to_owned() });
        sections.push((*title, lines.collect()));
    }
    sections
}

/// The shortcut reference, while it is open: a flat panel over the window
/// listing every binding of the table and the mouse gestures.
pub fn window(ctx: &egui::Context, cx: &mut Cx) {
    if !cx.state.shortcuts_open {
        return;
    }
    let theme = cx.theme;
    let sections = reference();
    // Sections flow into three columns of about equal height.
    let heights: Vec<f32> = sections.iter().map(|(_, lines)| HEADING + lines.len() as f32 * ROW + 10.0).collect();
    let target = heights.iter().sum::<f32>() / 3.0;
    let mut columns: Vec<Vec<usize>> = vec![Vec::new()];
    let mut filled = 0.0;
    for (index, height) in heights.iter().enumerate() {
        if filled > 0.0 && filled + height / 2.0 > target && columns.len() < 3 {
            columns.push(Vec::new());
            filled = 0.0;
        }
        filled += height;
        columns.last_mut().expect("there is always a column").push(index);
    }
    let tallest = columns.iter().map(|column| column.iter().map(|index| heights[*index]).sum::<f32>());
    let body_height = tallest.fold(0.0, f32::max);
    let width = columns.len() as f32 * COLUMN + (columns.len() - 1) as f32 * COLUMN_GAP;

    let frame = egui::Frame::NONE
        .fill(theme.panel)
        .stroke(egui::Stroke::new(1.0, theme.hairline))
        .inner_margin(egui::Margin::symmetric(18, 14));
    let modal = egui::Modal::new(egui::Id::unique("shortcut-reference"))
        .frame(frame)
        .backdrop_color(theme.backdrop)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let (title, _) = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover());
            text::paint(ui.painter(), title, Align2::LEFT_CENTER, theme.text.caps, "Shortcuts", theme.ink);
            text::paint(ui.painter(), title, Align2::RIGHT_CENTER, theme.text.micro, "Esc closes", theme.ink_faint);
            ui.add_space(8.0);
            let (rule, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
            paint::hline(ui.painter(), rule.x_range(), rule.top(), theme.hairline);
            ui.add_space(6.0);

            // A window too short for the list scrolls it. The list is given
            // its place outright: left to size itself, a scroll area inside
            // a dialog settles for less room than the window has.
            let room = (ui.ctx().content_rect().height() - 150.0).max(160.0);
            let (list, _) = ui.allocate_exact_size(vec2(width, body_height.min(room)), Sense::hover());
            let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(list));
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut ui, |ui| {
                let (body, _) = ui.allocate_exact_size(vec2(width, body_height), Sense::hover());
                let painter = ui.painter();
                for (column, members) in columns.iter().enumerate() {
                    let left = body.left() + column as f32 * (COLUMN + COLUMN_GAP);
                    let mut top = body.top();
                    for index in members {
                        let (title, lines) = &sections[*index];
                        let heading = Rect::from_min_size(pos2(left, top), vec2(COLUMN, HEADING));
                        let label = heading.with_min_y(heading.top() + 6.0).with_max_y(heading.bottom() - 3.0);
                        text::paint(painter, label, Align2::LEFT_CENTER, theme.text.micro, title, theme.ink_faint);
                        paint::hline_above(painter, heading.x_range(), heading.bottom(), theme.hairline);
                        top = heading.bottom();
                        for line in lines {
                            let row = Rect::from_min_size(pos2(left, top), vec2(COLUMN, ROW));
                            let used = text::paint(
                                painter,
                                row,
                                Align2::RIGHT_CENTER,
                                theme.text.value,
                                &line.keys,
                                theme.ink,
                            );
                            let room = row.with_max_x(row.right() - used - 10.0);
                            let style = trill_noir::TextStyle { size: 12.0, ..theme.text.body };
                            text::paint(painter, room, Align2::LEFT_CENTER, style, &line.does, theme.ink_dim);
                            top += ROW;
                        }
                        top += 10.0;
                    }
                }
            });
        });
    if modal.should_close() {
        cx.state.shortcuts_open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_chord_is_bound_twice_in_the_same_circumstances() {
        for (index, binding) in BINDINGS.iter().enumerate() {
            for other in &BINDINGS[index + 1..] {
                let same_chord = binding.modifiers == other.modifiers && binding.key == other.key;
                assert!(!same_chord, "{} is bound twice", chord_label(binding.modifiers, binding.key));
            }
        }
    }

    #[test]
    fn every_binding_is_documented_and_every_command_named_once() {
        for binding in BINDINGS {
            assert!(!binding.does.is_empty(), "{:?} has no description", binding.command);
            // One wording per command, so the reference can merge its keys.
            let wordings: Vec<&str> =
                BINDINGS.iter().filter(|other| other.command == binding.command).map(|other| other.does).collect();
            assert!(wordings.iter().all(|does| *does == binding.does), "{:?}: {wordings:?}", binding.command);
        }
    }

    #[test]
    fn chords_read_the_way_menus_print_them() {
        assert_eq!(keys(Command::Undo), "Ctrl+Z");
        assert_eq!(keys(Command::Redo), "Ctrl+Shift+Z", "the first binding is the one a menu shows");
        assert_eq!(keys(Command::Delete), "Del");
        assert_eq!(keys(Command::TogglePlay), "Space");
        assert_eq!(keys(Command::MuteClips), "0");
        assert_eq!(keys(Command::Nudge(-1, 0)), "Left");
        assert_eq!(keys(Command::DeleteTime), "Ctrl+Shift+Del");
        assert_eq!(keys(Command::Nudge(9, 9)), "", "a command without a binding has no keys");
    }

    #[test]
    fn the_reference_lists_every_binding_under_its_section() {
        let sections = reference();
        let listed: usize = sections.iter().take(Section::ALL.len()).map(|(_, lines)| lines.len()).sum();
        let distinct: std::collections::HashSet<(&str, &str)> =
            BINDINGS.iter().map(|binding| (binding.section.title(), binding.does)).collect();
        assert_eq!(listed, distinct.len());

        let (_, edit) = sections.iter().find(|(title, _)| *title == "Edit").unwrap();
        let redo = edit.iter().find(|line| line.does == "Redo").unwrap();
        assert_eq!(redo.keys, "Ctrl+Shift+Z / Ctrl+Y", "both chords of a command share its line");
        let delete = edit.iter().find(|line| line.does == "Delete the selection").unwrap();
        assert_eq!(delete.keys, "Del / Backspace");
        assert!(sections.iter().any(|(title, lines)| *title == "Automation" && !lines.is_empty()));
    }

    /// Runs one frame in which `events` arrive and returns what `read` makes
    /// of them.
    fn commands(modifiers: Modifiers, events: Vec<Event>, typing: bool, selection: bool) -> Vec<Command> {
        let ctx = egui::Context::default();
        let mut events = events;
        events.insert(0, Event::ModifiersChanged(modifiers));
        let mut found = Vec::new();
        let input = egui::RawInput { events, focused: true, ..Default::default() };
        let mut output = ctx.run_ui(input, |ui| found = read(ui.ctx(), typing, selection));
        output.textures_delta.clear();
        found
    }

    fn press(modifiers: Modifiers, key: Key) -> Vec<Event> {
        vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }]
    }

    #[test]
    fn the_more_specific_chord_wins() {
        assert_eq!(commands(CTRL_SHIFT, press(CTRL_SHIFT, Key::Z), false, false), [Command::Redo]);
        assert_eq!(commands(CTRL, press(CTRL, Key::Z), false, false), [Command::Undo]);
        assert_eq!(commands(NONE, press(NONE, Key::Z), false, false), [Command::ZoomToSelection]);
        assert_eq!(commands(SHIFT, press(SHIFT, Key::Z), false, false), [Command::ZoomToSong]);
        assert_eq!(commands(CTRL, press(CTRL, Key::E), false, false), [Command::Split]);
        assert_eq!(commands(CTRL_SHIFT, press(CTRL_SHIFT, Key::E), false, false), [Command::ExportWav]);
        assert_eq!(commands(CTRL_SHIFT, press(CTRL_SHIFT, Key::D), false, false), [Command::DuplicateTime]);
    }

    #[test]
    fn typing_keeps_single_keys_and_arrows_wait_for_a_selection() {
        assert!(commands(NONE, press(NONE, Key::Space), true, false).is_empty());
        assert!(commands(CTRL, press(CTRL, Key::A), true, true).is_empty(), "Ctrl+A selects the text being typed");
        assert_eq!(commands(CTRL, press(CTRL, Key::S), true, false), [Command::Save], "saving works while typing");
        assert!(commands(NONE, press(NONE, Key::ArrowLeft), false, false).is_empty());
        assert_eq!(commands(NONE, press(NONE, Key::ArrowLeft), false, true), [Command::Nudge(-1, 0)]);
        assert_eq!(commands(SHIFT, press(SHIFT, Key::ArrowUp), false, true), [Command::Nudge(0, 12)]);
    }

    #[test]
    fn clipboard_events_stand_for_their_chords() {
        assert_eq!(commands(CTRL, vec![Event::Copy], false, false), [Command::Copy]);
        assert_eq!(commands(CTRL, vec![Event::Cut], false, false), [Command::Cut]);
        assert_eq!(commands(CTRL, vec![Event::Paste("anything".to_owned())], false, false), [Command::Paste]);
        assert!(commands(CTRL, vec![Event::Copy], true, false).is_empty(), "a text field copies its own text");
        // A backend that does deliver the key press is understood too.
        assert_eq!(commands(CTRL, press(CTRL, Key::V), false, false), [Command::Paste]);
    }
}
