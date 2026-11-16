//! The assistant's panel, driven through the window against a scripted back
//! end: no CLI is started, and nothing of the user's is read or written.
//! The script says what a turn reports; where a real turn would change the
//! song through the MCP server, the test edits the session itself, from
//! outside the window, as that server does.

use super::*;
use crate::ui::assistant::testing::{Ending, Item, Offer, Problem, Script, Sent, ToolState, ready};
use serde_json::json;
use trill_assistant::{
    ApiErrorKind, CliInstallation, CliVersion, Event, FailureReason, LoginMethod, MINIMUM_CLI_VERSION, Notice,
    SessionId, TokenUsage, TurnStart, TurnSummary, Unavailable,
};

const CTRL_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);

fn conversation_id(last: char) -> SessionId {
    SessionId::parse(&format!("39367d22-e40b-4a04-9358-33e91538ff0{last}")).unwrap()
}

fn started() -> Event {
    Event::Started(TurnStart { session_id: conversation_id('a'), model: "test-model".into(), api_key_source: None })
}

fn finished() -> Event {
    Event::Finished(TurnSummary {
        turns: 3,
        duration: Duration::from_millis(21_400),
        usage: TokenUsage::default(),
        estimated_cost_usd: Some(0.0312),
    })
}

fn text(piece: &str) -> Event {
    Event::Text(piece.to_owned())
}

fn call(id: &str, tool: &str, input: serde_json::Value) -> Event {
    Event::ToolCallStarted { id: id.to_owned(), tool: tool.to_owned(), input }
}

fn answer(id: &str, tool: &str, result: Result<&str, &str>) -> Event {
    Event::ToolCallFinished {
        id: id.to_owned(),
        tool: tool.to_owned(),
        result: result.map(str::to_owned).map_err(str::to_owned),
    }
}

impl Harness {
    /// Puts `script` behind the assistant and opens its panel with the
    /// keyboard shortcut.
    fn open_assistant(&mut self, script: &Script) {
        self.app.assistant_mut().use_script(script.clone());
        self.key(CTRL_SHIFT, Key::A);
        assert!(self.app.assistant().is_open());
        self.idle(2);
    }

    /// Types `message` into the message field and presses Enter.
    fn tell(&mut self, message: &str) {
        let field = self.app.assistant().field();
        self.click(field.center());
        self.type_text(message);
        self.key(Modifiers::NONE, Key::Enter);
    }

    fn items(&self) -> &[Item] {
        &self.app.assistant().transcript().items
    }

    /// Whether something on screen is labeled exactly `label`. (The
    /// harness's own search also takes a label that merely begins so.)
    fn shows(&self, label: &str) -> bool {
        self.labels.iter().any(|(text, _)| text == label)
    }

    /// The assistant's reply so far, its passages joined.
    fn reply(&self) -> String {
        let passages = self.items().iter().filter_map(|item| match item {
            Item::Reply { text } => Some(text.as_str()),
            _ => None,
        });
        passages.collect::<Vec<_>>().join(" | ")
    }

    /// How the last turn ended.
    fn ending(&self) -> (Ending, bool) {
        match self.items().last() {
            Some(Item::End(end)) => (end.ending.clone(), end.changed),
            other => panic!("the conversation does not end with a turn's end: {other:?}"),
        }
    }
}

/// Three tracks, the assistant open on a script that finds it ready.
fn assisted() -> (Harness, Script, [TrackId; 3], [ClipId; 3]) {
    let (mut harness, tracks, clips) = with_three_tracks(UiOptions::default());
    let script = Script::ready();
    harness.open_assistant(&script);
    (harness, script, tracks, clips)
}

// ---- the dock ---------------------------------------------------------------

#[test]
fn closed_and_idle_the_assistant_does_nothing() {
    let (mut harness, ..) = with_three_tracks(UiOptions::default());
    let script = Script::ready();
    harness.app.assistant_mut().use_script(script.clone());
    harness.idle(30);
    assert!(!harness.app.assistant().is_open(), "the panel is closed until it is asked for");
    assert!(!harness.app.assistant().wants_tick(), "and then nothing of it runs in a frame");
    assert_eq!(script.state().checks, 0, "nobody looks for the CLI before the panel is opened");
    assert!(!harness.is_on_screen("Send"));

    // The button in the top bar opens it, and only then is the CLI looked for.
    harness.click_label("Assistant");
    harness.idle(2);
    assert!(harness.app.assistant().is_open());
    assert_eq!(script.state().checks, 1);
    assert!(harness.is_on_screen("Send"));
    assert!(harness.is_on_screen("Message to the assistant"));

    // The shortcut and the View menu put it away and bring it back.
    harness.key(CTRL_SHIFT, Key::A);
    assert!(!harness.app.assistant().is_open());
    assert!(!harness.is_on_screen("Send"));
    harness.click_label("View");
    harness.click_last_label("Assistant");
    assert!(harness.app.assistant().is_open());
    assert_eq!(script.state().checks, 1, "it was found once; opening the panel again does not look again");
    harness.click_label("Close the assistant");
    assert!(!harness.app.assistant().is_open());
    assert!(!harness.app.assistant().wants_tick());
}

#[test]
fn the_panel_is_resized_by_its_edge_and_keeps_the_rest_of_the_window() {
    let (mut harness, ..) = assisted();
    let width = harness.app.assistant().width();
    assert_eq!(width, crate::ui::assistant::DEFAULT_WIDTH);
    // The edge is grabbed a touch inside the panel.
    let edge = pos2(SCREEN.x - width + 2.0, 400.0);
    harness.drag(edge, edge - vec2(80.0, 0.0));
    let wider = harness.app.assistant().width();
    assert!((wider - (width + 80.0)).abs() < 3.0, "dragged 80 points wider: {wider}");

    // Dragged across the whole window, it stops where the rest still fits.
    let edge = pos2(SCREEN.x - wider + 2.0, 400.0);
    harness.drag(edge, pos2(100.0, 400.0));
    let widest = harness.app.assistant().width();
    assert!(widest <= crate::ui::assistant::MAX_WIDTH + 1.0, "{widest}");
    harness.drag(pos2(SCREEN.x - widest + 2.0, 400.0), pos2(SCREEN.x - 20.0, 400.0));
    let narrowest = harness.app.assistant().width();
    assert!((narrowest - crate::ui::assistant::MIN_WIDTH).abs() < 1.0, "{narrowest}");
}

#[test]
fn the_panel_is_remembered_from_one_session_to_the_next() {
    let home = tempfile::tempdir().unwrap();
    let start = || {
        let storage = trill_runtime::Storage::At { settings: home.path().join("settings.json"), library_index: None };
        Arc::new(Runtime::start(RuntimeOptions { storage, ..RuntimeOptions::hermetic() }).unwrap())
    };
    let kept = home.path().join("assistant.json");

    let mut harness = Harness::on(start(), UiOptions::default(), |_| {});
    assert!(!harness.app.assistant().is_open());
    harness.idle(30);
    drop(harness);
    assert!(!kept.exists(), "nothing is written for having looked");

    let mut harness = Harness::on(start(), UiOptions::default(), |_| {});
    harness.open_assistant(&Script::ready());
    let edge = pos2(SCREEN.x - harness.app.assistant().width() + 2.0, 400.0);
    harness.drag(edge, edge - vec2(60.0, 0.0));
    let width = harness.app.assistant().width().round();
    drop(harness);
    assert!(kept.exists());

    let harness = Harness::on(start(), UiOptions::default(), |_| {});
    assert!(harness.app.assistant().is_open(), "open, as it was left");
    assert_eq!(harness.app.assistant().width().round(), width);
}

// ---- 1: it knows what the user is looking at --------------------------------------

#[test]
fn a_message_goes_with_what_the_user_is_looking_at() {
    let (mut harness, script, tracks, clips) = assisted();
    harness.session.lock().edit(|project| project.set_loop(true, 8.0, 24.0)).unwrap();
    // Select clip B on the first track, and put the playhead at bar 3.
    harness.click(harness.lane(17.0, 0));
    harness.click(harness.ruler(8.0, false));
    harness.wait_for("the seek", |session| (session.transport().position_beats - 8.0).abs() < 1e-9);
    harness.idle(2);
    assert!(harness.is_on_screen("Sees One / B"), "the panel shows what the assistant will be told");

    harness.tell("make this busier");
    let sent = script.state().sent.clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].message, "make this busier");
    let context = sent[0].context.clone().expect("what the user is looking at goes along");
    let line = |key: &str| {
        let found = context.lines().find(|line| line.starts_with(key));
        found.unwrap_or_else(|| panic!("no '{key}' in:\n{context}")).to_owned()
    };
    assert_eq!(line("project:"), "project: \"Untitled\", 120 BPM, 4/4 time (a bar is 4 beats)");
    assert_eq!(line("playhead:"), "playhead: beat 8 (3.1.1), stopped");
    assert_eq!(line("loop:"), "loop: on, beats 8 to 24 (3.1.1 to 7.1.1)");
    assert_eq!(line("selected track:"), format!("selected track: \"One\" ({})", tracks[0]));
    assert_eq!(
        line("selected clip:"),
        format!("selected clip: \"B\" ({}, on \"One\", beats 16 to 20 (5.1.1 to 6.1.1), MIDI, 1 note)", clips[1])
    );
    assert!(line("detail panel:").starts_with("detail panel: the Clip tab"));
    assert_eq!(line("time selection:"), "time selection: none");
    assert_eq!(line("edit cursor:"), "edit cursor: beat 8 (3.1.1)");
    assert!(harness.app.assistant().draft().is_empty(), "the field is cleared for the next message");
    assert_eq!(harness.items(), [Item::User { text: "make this busier".into() }]);

    // The next message goes with what is looked at then: another track,
    // its device, the mixer, and a stretch of time.
    script.say([started(), text("Done."), finished()]);
    harness.idle(2);
    harness.click(harness.lane(2.0, 1));
    harness.click(harness.labeled_below("Devices", harness.detail_top()));
    harness.drag(harness.ruler(4.0, false), harness.ruler(12.0, false));
    harness.tell("and this?");
    let context = script.state().sent[1].context.clone().unwrap();
    assert!(context.contains(&format!("selected track: \"Two\" ({})", tracks[1])), "{context}");
    assert!(context.contains("selected clips: none"), "{context}");
    assert!(context.contains("detail panel: the Devices tab"), "{context}");
    assert!(context.contains("time selection: beats 4 to 12 (2.1.1 to 4.1.1) on every track"), "{context}");
    assert!(!context.contains("selected clip: \"B\""), "what was selected before is not what is selected now");
}

// ---- 2: one request, one undo step -------------------------------------------------

#[test]
fn everything_a_turn_changes_is_one_undo_step_with_an_undo_of_its_own() {
    let (mut harness, script, tracks, _) = assisted();
    let before = harness.project();
    let depth = harness.undo_depth();

    harness.tell("add a bass part");
    script.say([started(), call("1", "create_track", json!({ "name": "Bass" }))]);
    harness.idle(2);
    // What the tools do, as the MCP server does it: edits from outside the
    // window, some of them wrapped in a gesture of the tool's own.
    let bass = harness.session.lock().edit(|project| Ok(project.add_track("Bass"))).unwrap();
    script.say([answer("1", "create_track", Ok("{}")), call("2", "update_track", json!({ "track": bass.0 }))]);
    harness.idle(2);
    {
        let mut session = harness.session.lock();
        session.begin_gesture();
        session.edit(|project| project.set_track_volume(bass, -6.0)).unwrap();
        session.edit(|project| project.set_track_pan(bass, 0.2)).unwrap();
        session.end_gesture();
    }
    // Meanwhile the user drags a fader in the window. Letting go of it ends
    // a gesture, which must not end the turn's group.
    harness.drag(harness.volume_bar(0, 0.84), harness.volume_bar(0, 0.4));
    assert!(harness.project().track(tracks[0]).unwrap().volume_db < -10.0);
    script.say([answer("2", "update_track", Ok("{}")), call("3", "create_clip", json!({ "track": bass.0 }))]);
    harness.idle(2);
    harness.session.lock().edit(|project| project.add_midi_clip(bass, "Line", 0.0, 8.0)).unwrap();
    script.say([answer("3", "create_clip", Ok("{}")), text("Added a bass part."), finished()]);
    harness.idle(3);

    let after = harness.project();
    assert_eq!(after.tracks.len(), 4);
    assert_eq!(harness.undo_depth(), depth + 1, "the whole turn, the fader drag in it included, is one step");
    assert_eq!(
        harness.ending(),
        (Ending::Finished { seconds: 21.4, steps: 3, estimated_cost: Some(0.0312) }, true),
        "the turn is marked as having changed the song"
    );
    assert!(
        harness.is_on_screen("Changed the song \u{00B7} 21 s \u{00B7} est. $0.03"),
        "the cost reads as an estimate"
    );

    // The turn's own Undo takes all of it back, and Redo brings it back.
    harness.click_label("Undo");
    assert_eq!(harness.project(), before);
    harness.idle(2);
    assert!(harness.is_on_screen("Undone") && !harness.shows("Undo"));
    harness.click_label("Redo");
    assert_eq!(harness.project(), after);
    harness.idle(2);

    // Ctrl+Z does the same, as for any other step.
    harness.click(harness.lane(2.0, 2));
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(harness.project(), before);
    harness.key(CTRL_SHIFT, Key::Z);
    assert_eq!(harness.project(), after);

    // Once something else has changed the song, the turn's Undo would take
    // that back instead, so it is no longer offered.
    harness.idle(2);
    assert!(harness.shows("Undo"));
    harness.session.lock().edit(|project| project.set_tempo(90.0)).unwrap();
    harness.idle(3);
    assert!(!harness.shows("Undo") && !harness.shows("Redo"));
    assert!(harness.is_on_screen("Changed the song"));
}

#[test]
fn a_stopped_turn_closes_its_group_and_nothing_is_sent_until_it_has_stopped() {
    let (mut harness, script, tracks, _) = assisted();
    let before = harness.project();
    let depth = harness.undo_depth();

    harness.tell("double everything");
    script.say([started(), text("Working on it"), call("1", "update_track", json!({ "track": tracks[0].0 }))]);
    harness.idle(2);
    harness.session.lock().edit(|project| project.set_track_volume(tracks[0], -3.0)).unwrap();
    harness.session.lock().edit(|project| project.set_track_pan(tracks[0], -0.3)).unwrap();
    harness.idle(2);
    assert!(harness.app.assistant().is_working());
    assert!(harness.is_on_screen("Stop") && !harness.is_on_screen("Send"));

    // The lower of the two: the transport has a Stop of its own.
    harness.click_last_label("Stop");
    assert_eq!(script.state().cancels, 1);
    assert!(harness.app.assistant().is_stopping());
    harness.idle(2);
    assert!(harness.is_on_screen("Stopping\u{2026}"));
    // The back end has not said it stopped: the turn is still under way,
    // its group still open, and a message typed now waits.
    harness.click_last_label("Stop");
    harness.tell("never mind, try this");
    assert_eq!(script.state().cancels, 1, "one stop is enough");
    assert_eq!(script.state().sent.len(), 1, "nothing is sent while the turn is stopping");
    assert_eq!(harness.app.assistant().draft(), "never mind, try this", "what was typed is kept");
    assert!(harness.app.assistant().is_working());

    script.say([Event::Cancelled]);
    harness.idle(2);
    assert!(!harness.app.assistant().is_working());
    assert_eq!(harness.ending(), (Ending::Stopped, true));
    let Some(Item::Tool(cut)) = harness.items().iter().find(|item| matches!(item, Item::Tool(_))) else {
        panic!("the tool call is in the conversation")
    };
    assert_eq!(cut.state, ToolState::Stopped, "the call that was under way is marked as cut short");
    assert_eq!(harness.undo_depth(), depth + 1, "what was changed before the stop is one step");

    // The group is closed: an edit made now is a step of its own.
    harness.session.lock().edit(|project| project.set_tempo(100.0)).unwrap();
    assert_eq!(harness.undo_depth(), depth + 2);
    harness.session.lock().undo();
    harness.idle(3);
    harness.click_label("Undo");
    assert_eq!(harness.project(), before, "and the turn's Undo takes back what it had done");

    // Now the message that waited can go.
    harness.click(harness.app.assistant().field().center());
    harness.key(Modifiers::NONE, Key::Enter);
    assert_eq!(script.state().sent.len(), 2);
    assert_eq!(script.state().sent[1].message, "never mind, try this");
}

#[test]
fn a_failed_turn_closes_its_group_too() {
    let (mut harness, script, tracks, _) = assisted();
    let depth = harness.undo_depth();
    harness.tell("pan everything left");
    script.say([started(), call("1", "update_track", json!({ "track": tracks[1].0, "pan": -1 }))]);
    harness.idle(2);
    harness.session.lock().edit(|project| project.set_track_pan(tracks[1], -1.0)).unwrap();
    harness.session.lock().edit(|project| project.set_track_pan(tracks[2], -1.0)).unwrap();
    let failure = FailureReason::Api { kind: ApiErrorKind::Server, message: "Overloaded".into() };
    script.say([answer("1", "update_track", Ok("{}")), Event::Failed(failure.clone())]);
    harness.idle(3);

    assert!(!harness.app.assistant().is_working());
    assert_eq!(harness.ending(), (Ending::Failed { text: failure.to_string(), offer: Offer::TryAgain }, true));
    assert!(harness.is_on_screen("Claude Code could not complete the request: Overloaded"));
    assert_eq!(harness.undo_depth(), depth + 1);
    harness.session.lock().edit(|project| project.set_tempo(100.0)).unwrap();
    assert_eq!(harness.undo_depth(), depth + 2, "the group did not stay open");
}

#[test]
fn a_turn_that_changes_nothing_leaves_no_step_and_offers_no_undo() {
    let (mut harness, script, ..) = assisted();
    let depth = harness.undo_depth();
    let step = harness.session.lock().undo_step();

    harness.tell("what key is this in?");
    script.say([
        started(),
        call("1", "analyze_music", json!({})),
        answer("1", "analyze_music", Ok("{\"key\":\"C major\"}")),
        text("C major."),
        finished(),
    ]);
    harness.idle(3);
    assert!(!harness.ending().1);
    assert_eq!(harness.undo_depth(), depth);
    assert_eq!(harness.session.lock().undo_step(), step, "the history is as it was");
    assert!(!harness.shows("Undo") && !harness.is_on_screen("Changed the song"));
    assert!(harness.is_on_screen("21 s \u{00B7} est. $0.03"));

    // Nor does a turn that was stopped before it did anything, or one that
    // could not start.
    harness.tell("go on");
    harness.click_last_label("Stop");
    script.say([Event::Cancelled]);
    harness.idle(2);
    harness.tell("go on");
    script.say([Event::Failed(FailureReason::CannotStart { detail: "access denied".into() })]);
    harness.idle(2);
    assert_eq!(harness.undo_depth(), depth);
    assert_eq!(harness.session.lock().undo_step(), step);
}

// ---- 3: it can be watched and stopped ------------------------------------------------

#[test]
fn the_reply_appears_as_it_is_written_and_each_tool_call_is_a_line() {
    let (mut harness, script, tracks, _) = assisted();
    harness.tell("add drums");
    assert!(harness.is_on_screen("Working\u{2026}"), "the turn shows as under way before anything has come back");

    script.say([started(), text("Let me ")]);
    harness.idle(1);
    assert_eq!(harness.reply(), "Let me ");
    script.say([text("look at "), text("the song.")]);
    harness.idle(1);
    assert_eq!(harness.reply(), "Let me look at the song.");
    assert!(harness.is_on_screen("Let me look at the song."));

    // A call shows as it starts, in words, and again as it finishes.
    script.say([call("1", "create_track", json!({ "name": "Drums", "instrument": "builtin:drumkit" }))]);
    harness.idle(1);
    assert!(harness.is_on_screen("Adding the track \u{201C}Drums\u{201D}\u{2026}"));
    // The song changes while the turn is still going, and the window shows it.
    harness.session.lock().edit(|project| Ok(project.add_track("Drums"))).unwrap();
    harness.idle(1);
    assert_eq!(harness.app.snapshot().project.tracks.len(), 4);
    assert!(harness.app.assistant().is_working());
    script.say([answer("1", "create_track", Ok("{ \"track_id\": 9 }"))]);
    harness.idle(1);
    assert!(harness.is_on_screen("Added the track \u{201C}Drums\u{201D}"));
    assert!(!harness.is_on_screen("Adding the track"));

    // Things are called by their names, and a call that fails is marked.
    script.say([
        call("2", "update_track", json!({ "track": tracks[0].0, "volume_db": -4 })),
        answer("2", "update_track", Ok("{}")),
        call("3", "write_drums", json!({ "target": { "track": 999 }, "groove": "house" })),
        answer("3", "write_drums", Err("track 999 was not found; the tracks are 1, 2, 3 and 9")),
        text("The first try missed; "),
    ]);
    harness.idle(1);
    assert!(harness.is_on_screen("Changed the volume of \u{201C}One\u{201D}"));
    assert!(harness.is_on_screen("Wrote drums on track 999: failed"));
    let states: Vec<ToolState> = harness
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Tool(line) => Some(line.state),
            _ => None,
        })
        .collect();
    assert_eq!(states, [ToolState::Done, ToolState::Done, ToolState::Failed]);
    assert_eq!(
        harness.reply(),
        "Let me look at the song. | The first try missed; ",
        "text after tools is a new passage"
    );

    // What went in and what came back are a click away.
    harness.click_label("Wrote drums on track 999: failed");
    let Some(Item::Tool(failed)) = harness.items().iter().rev().find(|item| matches!(item, Item::Tool(_))) else {
        panic!("a tool line")
    };
    assert!(failed.open);
    assert_eq!(failed.tool, "write_drums");
    assert!(failed.input.contains("\"groove\": \"house\""), "{}", failed.input);
    assert_eq!(failed.output, "track 999 was not found; the tracks are 1, 2, 3 and 9");
    harness.click_label("Wrote drums on track 999: failed");
    let Some(Item::Tool(failed)) = harness.items().iter().rev().find(|item| matches!(item, Item::Tool(_))) else {
        panic!("a tool line")
    };
    assert!(!failed.open, "and a click folds them away again");

    // Notices are part of the conversation.
    script.say([Event::Notice(Notice::PermissionDenied { tool: "render".into() }), text("done."), finished()]);
    harness.idle(2);
    assert!(harness.is_on_screen("The assistant was not allowed to use render."));
    assert!(!harness.app.assistant().is_working());
    assert!(harness.is_on_screen("Send"));
}

// ---- the message field ---------------------------------------------------------------

#[test]
fn enter_sends_shift_enter_starts_a_line_and_the_arrows_recall() {
    let (mut harness, script, ..) = assisted();
    let field = harness.app.assistant().field();
    harness.click(field.center());
    harness.type_text("first line");
    harness.key(Modifiers::SHIFT, Key::Enter);
    assert_eq!(harness.app.assistant().draft(), "first line\n");
    assert!(script.state().sent.is_empty(), "Shift+Enter does not send");
    harness.type_text("second line");
    // Space and the letters are the field's while it is typed in.
    harness.type_text(" a b");
    harness.key(Modifiers::NONE, Key::Space);
    assert!(!harness.session.lock().transport().playing, "Space is typed, not played");
    harness.key(Modifiers::NONE, Key::Enter);
    assert_eq!(script.state().sent[0].message, "first line\nsecond line a b");
    assert_eq!(harness.app.assistant().draft(), "");
    script.say([started(), finished()]);
    harness.idle(2);

    // An empty message is not one.
    harness.key(Modifiers::NONE, Key::Enter);
    harness.type_text("   ");
    harness.key(Modifiers::NONE, Key::Enter);
    assert_eq!(script.state().sent.len(), 1);
    harness.key(Modifiers::COMMAND, Key::A);
    harness.key(Modifiers::NONE, Key::Backspace);
    assert_eq!(harness.app.assistant().draft(), "");

    harness.type_text("another");
    harness.key(Modifiers::NONE, Key::Enter);
    script.say([started(), finished()]);
    harness.idle(2);

    // Up brings back what was sent, the last first; Down goes forward
    // again and past the last to an empty field.
    harness.key(Modifiers::NONE, Key::ArrowUp);
    assert_eq!(harness.app.assistant().draft(), "another");
    harness.key(Modifiers::NONE, Key::ArrowUp);
    assert_eq!(harness.app.assistant().draft(), "first line\nsecond line a b");
    harness.key(Modifiers::NONE, Key::ArrowUp);
    assert_eq!(harness.app.assistant().draft(), "first line\nsecond line a b", "there is nothing before the first");
    harness.key(Modifiers::NONE, Key::ArrowDown);
    assert_eq!(harness.app.assistant().draft(), "another");
    harness.key(Modifiers::NONE, Key::ArrowDown);
    assert_eq!(harness.app.assistant().draft(), "");

    // What is brought back can be changed and sent as a new message.
    harness.key(Modifiers::NONE, Key::ArrowUp);
    harness.type_text(" one");
    harness.key(Modifiers::NONE, Key::Enter);
    assert_eq!(script.state().sent.last().unwrap().message, "another one");
    assert_eq!(harness.undo_depth(), 1, "none of this touched the song");
}

#[test]
fn a_new_conversation_starts_over() {
    let (mut harness, script, ..) = assisted();
    harness.tell("hello");
    script.say([started(), text("Hello."), finished()]);
    harness.idle(2);
    assert_eq!(harness.app.assistant().conversation(), Some(&conversation_id('a')));
    harness.tell("again");
    script.say([started(), finished()]);
    harness.idle(2);
    assert_eq!(script.state().opened, [None], "one conversation, opened once, carries both messages");

    // Not while the assistant is answering.
    harness.tell("and a third");
    harness.click_label("New conversation");
    assert!(!harness.items().is_empty());
    script.say([started(), finished()]);
    harness.idle(2);

    harness.click_label("New conversation");
    assert!(harness.items().is_empty());
    assert_eq!(harness.app.assistant().conversation(), None);
    assert!(harness.is_on_screen("What is in this song, and what key is it in?"), "the panel is as it first was");
    harness.tell("fresh start");
    assert_eq!(script.state().opened, [None, None], "the next message opens a conversation that resumes nothing");

    // An example fills the field; it is not sent until the user sends it.
    script.say([started(), finished()]);
    harness.idle(2);
    harness.click_label("New conversation");
    harness.click_label("Make the selected clip busier.");
    assert_eq!(harness.app.assistant().draft(), "Make the selected clip busier.");
    assert_eq!(script.state().sent.len(), 4);
}

// ---- every state that is not ready ---------------------------------------------------------

/// Opens the panel on a script that finds `problem`, and checks that the
/// panel says so in the back end's words, offers to look again and has no
/// message field. Then lets the next look succeed.
fn explains(problem: Problem, heading: &str) {
    let (mut harness, ..) = with_three_tracks(UiOptions::default());
    let script = Script::unavailable(problem.clone());
    harness.open_assistant(&script);
    let words = format!("{heading}. {problem}");
    assert!(harness.is_on_screen(&words), "'{words}' is not on screen: {:?}", harness.labels);
    assert!(harness.is_on_screen("Check again"));
    assert!(!harness.is_on_screen("Send") && !harness.is_on_screen("Message to the assistant"));
    for forbidden in ["Sign in", "Log in", "Password", "API key", "Token"] {
        assert!(!harness.labels.iter().any(|(label, _)| label == forbidden), "the panel offers no '{forbidden}'");
    }

    // Looking again finds the same thing until it has been put right.
    harness.click_label("Check again");
    harness.idle(2);
    assert_eq!(script.state().checks, 2);
    assert!(harness.is_on_screen(&words));
    script.answer(Ok(ready()));
    harness.click_label("Check again");
    harness.idle(2);
    assert_eq!(script.state().checks, 3);
    assert!(harness.is_on_screen("Send") && !harness.is_on_screen("Check again"));
    harness.tell("hello");
    assert_eq!(script.state().sent.len(), 1);
}

#[test]
fn a_missing_cli_is_explained() {
    explains(Problem::Cli(Unavailable::NotInstalled), "Claude Code is not installed");
}

#[test]
fn a_cli_that_is_too_old_is_explained() {
    let installation = CliInstallation { path: "claude".into(), version: CliVersion::new(2, 0, 14) };
    let problem = Problem::Cli(Unavailable::TooOld { installation, minimum: MINIMUM_CLI_VERSION });
    assert!(problem.to_string().contains("2.0.14") && problem.to_string().contains("claude update"));
    explains(problem, "Claude Code is too old");
}

#[test]
fn a_cli_that_does_not_work_is_explained() {
    let detail = "it did not report a version".to_owned();
    explains(
        Problem::Cli(Unavailable::NotWorking { path: "C:/tools/claude.exe".into(), detail }),
        "Claude Code is not working",
    );
    explains(
        Problem::Cli(Unavailable::UnsupportedInstall { path: "C:/npm/claude.cmd".into() }),
        "Claude Code cannot be started",
    );
}

#[test]
fn not_being_signed_in_is_explained_and_never_offered_here() {
    let installation = CliInstallation { path: "claude".into(), version: CliVersion::new(2, 1, 325) };
    let problem = Problem::Cli(Unavailable::NotLoggedIn { installation });
    assert_eq!(
        problem.to_string(),
        "Claude Code is not signed in. Open a terminal, run `claude`, sign in there, then try again."
    );
    explains(problem, "Not signed in");
}

#[test]
fn an_mcp_server_that_is_not_running_is_explained() {
    explains(
        Problem::Mcp(
            "Trill was started with --no-mcp, so the MCP server the assistant works through is off. Start Trill \
             without --no-mcp to use the assistant."
                .into(),
        ),
        "The MCP server is not running",
    );
}

#[test]
fn a_run_that_keeps_nothing_says_the_assistant_is_not_available() {
    // The window as the tests and the screenshots start it, with no
    // script put in: there is no folder for the CLI to work in.
    let (mut harness, ..) = with_three_tracks(UiOptions::default());
    harness.key(CTRL_SHIFT, Key::A);
    harness.idle(2);
    assert!(harness.is_on_screen("Not available in this run."));
    assert!(harness.is_on_screen("Check again") && !harness.is_on_screen("Send"));
}

#[test]
fn looking_for_the_cli_does_not_hold_up_the_window() {
    let (mut harness, ..) = with_three_tracks(UiOptions::default());
    let script = Script::ready();
    script.state().hold_answer = true;
    harness.open_assistant(&script);
    assert!(harness.is_on_screen("Looking for Claude Code\u{2026}."));
    assert!(!harness.is_on_screen("Send") && !harness.is_on_screen("Check again"));
    // The window is in use meanwhile.
    harness.click(harness.lane(9.0, 0));
    assert!(harness.app.state().selected_clip.is_some());
    harness.idle(20);
    assert_eq!(script.state().checks, 1, "it is asked once, not once a frame");
    script.state().hold_answer = false;
    harness.idle(2);
    assert!(harness.is_on_screen("Send"));
}

#[test]
fn a_usage_limit_is_relayed_in_the_clis_words_with_a_way_to_try_again() {
    let (mut harness, script, ..) = assisted();
    harness.tell("add a pad");
    let reason = FailureReason::UsageLimitReached {
        message: "You've hit your session limit \u{00B7} resets 3:00pm".into(),
        resets_at: None,
    };
    script.say([started(), Event::Notice(Notice::UsageLimitNear { resets_at: None }), Event::Failed(reason.clone())]);
    harness.idle(3);
    assert!(harness.is_on_screen("You are close to your Claude usage limit."));
    assert!(harness.is_on_screen(
        "Your Claude usage limit has been reached. Claude Code said: You've hit your session limit \u{00B7} resets \
         3:00pm"
    ));
    assert_eq!(harness.ending(), (Ending::Failed { text: reason.to_string(), offer: Offer::TryAgain }, false));
    assert!(harness.is_on_screen("Send"), "the limit is the account's, not a fault here: the field stays");
    assert_eq!(script.state().checks, 1, "and there is nothing to look for again");

    harness.click_label("Try again");
    let sent = script.state().sent.clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].message, "add a pad", "the same message goes again");
    assert!(sent[1].context.is_some());
}

#[test]
fn a_turn_limit_offers_to_continue() {
    let (mut harness, script, tracks, _) = assisted();
    harness.tell("write the whole arrangement");
    script.say([started(), call("1", "update_track", json!({ "track": tracks[0].0 }))]);
    harness.idle(2);
    harness.session.lock().edit(|project| project.set_track_volume(tracks[0], -2.0)).unwrap();
    script.say([answer("1", "update_track", Ok("{}")), Event::Failed(FailureReason::TurnLimitReached { limit: 80 })]);
    harness.idle(3);
    assert!(harness.is_on_screen("The assistant stopped after 80 steps. Ask it to continue if it was not finished."));
    assert!(harness.ending().1, "what it did so far stays done, as one step");
    assert!(harness.shows("Undo"));

    harness.click_label("Continue");
    let sent = script.state().sent.clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].message, "Continue.");
    assert_eq!(script.state().opened, [None], "in the same conversation");
}

#[test]
fn a_turn_that_finds_the_cli_signed_out_has_the_panel_look_again() {
    let (mut harness, script, ..) = assisted();
    harness.tell("hello");
    // Signed out since the panel was opened.
    let installation = CliInstallation { path: "claude".into(), version: CliVersion::new(2, 1, 325) };
    script.answer(Err(Problem::Cli(Unavailable::NotLoggedIn { installation })));
    script.say([Event::Failed(FailureReason::NotLoggedIn)]);
    harness.idle(3);
    assert_eq!(script.state().checks, 2, "the failure had the panel look again by itself");
    assert!(harness.is_on_screen("Not signed in."));
    assert!(harness.is_on_screen("Check again") && !harness.is_on_screen("Send"));
    assert!(!harness.is_on_screen("Try again"), "nothing can be sent while nobody is signed in");

    // Signed in again, in the CLI: the panel finds out when asked, and the
    // message can go again.
    script.answer(Ok(ready()));
    harness.click_label("Check again");
    harness.idle(2);
    harness.click_label("Try again");
    assert_eq!(script.state().sent.len(), 2);
    assert_eq!(script.state().sent[1].message, "hello");
}

#[test]
fn a_server_that_stopped_answering_is_looked_for_again() {
    let (mut harness, script, ..) = assisted();
    harness.tell("hello");
    let reason =
        FailureReason::McpServerUnavailable { url: "http://127.0.0.1:7777/mcp".into(), status: "failed".into() };
    script.say([Event::Failed(reason.clone())]);
    harness.idle(3);
    assert!(harness.is_on_screen(&reason.to_string()));
    assert_eq!(script.state().checks, 2);
    // The look found it up again (a real one starts a server if it must).
    assert!(harness.is_on_screen("Send") && harness.is_on_screen("Try again"));
    harness.click_label("Try again");
    assert_eq!(script.state().opened.len(), 2, "the conversation is opened again, on the address that holds now");
}

#[test]
fn a_conversation_the_cli_no_longer_has_is_started_afresh() {
    let (mut harness, script, ..) = assisted();
    harness.tell("hello");
    script.say([started(), finished()]);
    harness.idle(2);
    assert!(harness.app.assistant().conversation().is_some());

    harness.tell("still there?");
    script.say([Event::Failed(FailureReason::SessionNotFound)]);
    harness.idle(2);
    assert!(harness.is_on_screen(&FailureReason::SessionNotFound.to_string()));
    assert_eq!(harness.app.assistant().conversation(), None, "the id is let go of");
    harness.click_label("Try again");
    assert_eq!(script.state().sent.last().unwrap().message, "still there?");
}

// ---- who is billed ---------------------------------------------------------------------

#[test]
fn a_conversation_billed_to_an_api_key_says_so() {
    let (mut harness, script, ..) = assisted();
    assert!(!harness.labels.iter().any(|(label, _)| label.contains("billed")));
    harness.tell("hello");
    let start = TurnStart {
        session_id: conversation_id('a'),
        model: "test-model".into(),
        api_key_source: Some("ANTHROPIC_API_KEY".into()),
    };
    script.say([Event::Started(start), text("Hello."), finished()]);
    harness.idle(3);
    let said = "Claude Code is using an API key (ANTHROPIC_API_KEY), so this conversation is billed to that API \
                account, not to your Claude login.";
    assert!(harness.is_on_screen(said), "{:?}", harness.labels);
    // It stays said for as long as the panel is up.
    harness.click_label("New conversation");
    assert!(harness.is_on_screen(said));

    // A CLI that is signed in with a key says so before anything is sent.
    let (mut harness, ..) = with_three_tracks(UiOptions::default());
    let mut signed_in_with_key = ready();
    signed_in_with_key.cli.login = LoginMethod::ApiKey;
    let script = Script::default();
    script.answer(Ok(signed_in_with_key));
    harness.open_assistant(&script);
    assert!(
        harness.is_on_screen(
            "Claude Code is signed in with an API key, so this conversation is billed to that API account."
        )
    );
}

// ---- conversations and projects ------------------------------------------------------------

/// Runs frames until the conversation kept on disk has been read back.
fn wait_for_the_conversation(harness: &mut Harness) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while harness.items().is_empty() {
        assert!(Instant::now() < deadline, "the kept conversation never arrived");
        std::thread::sleep(Duration::from_millis(2));
        harness.idle(1);
    }
}

#[test]
fn a_conversation_is_taken_up_again_after_a_restart_per_project() {
    let home = tempfile::tempdir().unwrap();
    let start = || {
        let storage = trill_runtime::Storage::At { settings: home.path().join("settings.json"), library_index: None };
        Arc::new(Runtime::start(RuntimeOptions { storage, ..RuntimeOptions::hermetic() }).unwrap())
    };
    let song = home.path().join("songs").join("night.trill");
    let other = home.path().join("songs").join("day.trill");

    // The first session: a saved project, and a conversation about it.
    let mut harness = Harness::on(start(), UiOptions::default(), |session| {
        three_tracks(session);
        session.save(&song).unwrap();
        session.save(&other).unwrap();
        session.load(&song).unwrap();
    });
    let script = Script::ready();
    harness.open_assistant(&script);
    harness.tell("remember the word marimba");
    script.say([
        started(),
        call("1", "get_project", json!({})),
        answer("1", "get_project", Ok("{}")),
        text("I will."),
        finished(),
    ]);
    harness.idle(3);
    // As it will read back: a call's id means nothing after its turn.
    let mut said = harness.items().to_vec();
    for item in &mut said {
        if let Item::Tool(line) = item {
            line.id.clear();
        }
    }
    assert_eq!(said.len(), 4);
    drop(harness);

    let kept = std::fs::read_to_string(home.path().join("assistant.json")).unwrap();
    assert!(kept.contains(conversation_id('a').as_str()), "{kept}");
    assert!(kept.contains("night.trill") && !kept.contains("marimba"), "the id is kept by project, the words apart");
    let project = std::fs::read_to_string(&song).unwrap();
    assert!(!project.contains(conversation_id('a').as_str()), "nothing of it goes into the project file");

    // The next session, the same project: the conversation is shown again
    // and the next message continues it.
    let mut harness = Harness::on(start(), UiOptions::default(), |session| session.load(&song).unwrap());
    let script = Script::ready();
    harness.app.assistant_mut().use_script(script.clone());
    assert!(harness.app.assistant().is_open(), "the panel is open, as it was left");
    wait_for_the_conversation(&mut harness);
    assert_eq!(harness.items(), said);
    assert!(harness.is_on_screen("Read the project") && harness.is_on_screen("I will."));
    assert!(!harness.shows("Undo"), "the history did not survive the restart, so neither does its Undo");
    harness.tell("what was the word?");
    assert_eq!(script.state().opened, [Some(conversation_id('a'))], "it resumes the conversation it stored");
    script.say([started(), text("Marimba."), finished()]);
    harness.idle(2);

    // Another project has no conversation; coming back brings this one back.
    harness.session.lock().load(&other).unwrap();
    harness.app.state_mut().reset_for_new_project();
    harness.idle(3);
    assert!(harness.items().is_empty());
    assert_eq!(harness.app.assistant().conversation(), None);
    harness.session.lock().load(&song).unwrap();
    harness.app.state_mut().reset_for_new_project();
    harness.idle(2);
    wait_for_the_conversation(&mut harness);
    assert_eq!(harness.items().len(), 7);
    assert_eq!(harness.app.assistant().conversation(), Some(&conversation_id('a')));

    // Starting over forgets it for good.
    harness.click_label("New conversation");
    drop(harness);
    let kept = std::fs::read_to_string(home.path().join("assistant.json")).unwrap();
    assert!(!kept.contains(conversation_id('a').as_str()), "{kept}");
    let transcripts = home.path().join("assistant").join("transcripts");
    assert_eq!(std::fs::read_dir(transcripts).unwrap().count(), 0);
}

#[test]
fn a_project_saved_for_the_first_time_keeps_its_conversation_and_a_new_one_starts_clean() {
    let home = tempfile::tempdir().unwrap();
    let storage = trill_runtime::Storage::At { settings: home.path().join("settings.json"), library_index: None };
    let runtime = Arc::new(Runtime::start(RuntimeOptions { storage, ..RuntimeOptions::hermetic() }).unwrap());
    let mut harness = Harness::on(runtime, UiOptions::default(), |session| {
        three_tracks(session);
    });
    let script = Script::ready();
    harness.open_assistant(&script);
    harness.tell("hello");
    script.say([started(), text("Hello."), finished()]);
    harness.idle(3);
    harness.app.assistant_mut().store_mut().flush();
    let kept = std::fs::read_to_string(home.path().join("assistant.json")).unwrap();
    assert!(!kept.contains(conversation_id('a').as_str()), "a project that was never saved has nowhere to be kept by");

    // Saved: the conversation is about that file from now on.
    let song = home.path().join("first.trill");
    harness.session.lock().save(&song).unwrap();
    harness.idle(3);
    assert_eq!(harness.items().len(), 3, "saving changes nothing in the panel");
    harness.app.assistant_mut().store_mut().flush();
    let kept = std::fs::read_to_string(home.path().join("assistant.json")).unwrap();
    assert!(kept.contains(conversation_id('a').as_str()) && kept.contains("first.trill"), "{kept}");

    // A new project is another song: the panel starts clean, and a turn
    // that was under way for the old one is stopped first.
    harness.tell("carry on");
    harness.session.lock().new_project();
    harness.app.state_mut().reset_for_new_project();
    harness.idle(2);
    assert_eq!(script.state().cancels, 1, "the turn about the project that is gone is stopped");
    assert_eq!(harness.items().len(), 4, "and the panel waits for it to end");
    script.say([Event::Cancelled]);
    harness.idle(3);
    assert!(harness.items().is_empty());
    assert_eq!(harness.app.assistant().conversation(), None);
    harness.tell("a new song");
    assert_eq!(script.state().opened.len(), 2);
    assert_eq!(script.state().opened[1], None, "which resumes nothing");
}

#[test]
fn what_was_sent_is_what_the_script_saw() {
    // The seam itself: a message and its context arrive as given.
    let (mut harness, script, ..) = assisted();
    harness.tell("  trim me  ");
    let sent: Sent = script.state().sent[0].clone();
    assert_eq!(sent.message, "trim me");
    assert!(sent.context.is_some_and(|context| context.starts_with("project: ")));
}
