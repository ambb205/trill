//! The assistant's panel, driven through the window against a scripted back
//! end: no CLI is started, and nothing of the user's is read or written.
//! The script says what a turn reports; where a real turn would change the
//! song through the MCP server, the test edits the session itself, from
//! outside the window, as that server does.

use super::*;
use crate::ui::assistant::testing::{
    AT_WORK, Ending, Item, Offer, Place, Problem, Script, Sent, Setup, ToolState, ready,
};
use crate::ui::timeline::TimeView;
use serde_json::json;
use trill_assistant::{
    ApiErrorKind, CliInstallation, CliVersion, Event, FailureReason, LoginMethod, MINIMUM_CLI_VERSION, Notice,
    PlanChange, PlanStep, Progress, SessionId, StepStatus, TokenUsage, TurnStart, TurnSummary, Unavailable, WebLookup,
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
        // The field is where the last frames put it, if they put it at all.
        self.idle(2);
        assert!(self.is_on_screen("Message to the assistant"), "there is no message field: {:?}", self.labels);
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
fn what_is_asked_said_and_planned_is_entered_in_the_songs_journal_as_it_comes() {
    use trill_core::journal::{Author, Entry, Outcome, What};
    use trill_engine::journal::acting;
    let (mut harness, script, ..) = assisted();
    let entries = |harness: &Harness| -> Vec<(Author, String)> {
        let session = harness.session.lock();
        session.journal().unwrap().entries().map(|entry: &Entry| (entry.by.clone(), entry.line())).collect()
    };
    let before = entries(&harness).len();

    harness.tell("add a bass part");
    script.say([started(), text("I\u{2019}ll add "), text("a track for it.")]);
    harness.idle(3);
    script.say([planned("1", "Add the track", None), stands("1", StepStatus::InProgress)]);
    harness.idle(3);
    // The reply stands in the journal from its first words, so that a call
    // made before the window has seen its end comes after it. The server
    // enters the call itself; here the test does, as the server would.
    script.say([call("1", "create_track", json!({ "name": "Bass" }))]);
    {
        let mut session = harness.session.lock();
        let call =
            session.begin_call(Author::Assistant, "create_track", "Added the track \u{201C}Bass\u{201D}").unwrap();
        {
            let _acting = acting(call);
            session.edit(|project| Ok(project.add_track("Bass"))).unwrap();
        }
        session.end_call(call, Outcome::default());
    }
    harness.idle(3);
    script.say([answer("1", "create_track", Ok("{\"track_id\":9}"))]);
    script.say([looks_up("2", WebLookup::Search { query: "walking bass".into() })]);
    harness.idle(2);
    script.say([found("2", Ok("Three results."))]);
    script.say([stands("1", StepStatus::Done), text("Added a bass track."), finished()]);
    harness.idle(4);

    let told = entries(&harness)[before..].to_vec();
    let (you, assistant) = (Author::Person, Author::Assistant);
    assert_eq!(
        told,
        [
            (you, "add a bass part".to_owned()),
            (assistant.clone(), "I\u{2019}ll add a track for it.".to_owned()),
            (assistant.clone(), "Plan: 0 of 1 done".to_owned()),
            (assistant.clone(), "Plan: 0 of 1 done".to_owned()),
            (assistant.clone(), "Added the track \u{201C}Bass\u{201D}".to_owned()),
            (assistant.clone(), "Searched the web for \u{201C}walking bass\u{201D}".to_owned()),
            (assistant.clone(), "Plan: 1 of 1 done".to_owned()),
            (assistant.clone(), "Added a bass track.".to_owned()),
            (assistant, "Finished in 21 s".to_owned()),
        ]
    );
    let session = harness.session.lock();
    let journal = session.journal().unwrap();
    assert!(
        journal.entries().any(|entry| matches!(&entry.what, What::Look(look) if look.tool.as_deref() == Some("web")))
    );
    assert_eq!(&journal.head().unwrap(), session.project(), "and the story leads to the song");
    // All of it is as it stays: nothing is left that a file would wait for.
    assert!(journal.to_text(true).contains("Finished in 21 s"));
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
    // It had used a tool, so what is offered is to carry on: asking again
    // would ask for what is done a second time.
    assert_eq!(harness.ending(), (Ending::Failed { text: failure.to_string(), offer: Offer::Continue }, true));
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
    assert!(harness.is_on_screen("Writing drums on track 999: failed"));
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
    harness.click_label("Writing drums on track 999: failed");
    let Some(Item::Tool(failed)) = harness.items().iter().rev().find(|item| matches!(item, Item::Tool(_))) else {
        panic!("a tool line")
    };
    assert!(failed.open);
    assert_eq!(failed.tool, "write_drums");
    assert!(failed.input.contains("\"groove\": \"house\""), "{}", failed.input);
    assert_eq!(failed.output, "track 999 was not found; the tracks are 1, 2, 3 and 9");
    harness.click_label("Writing drums on track 999: failed");
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

// ---- 4: it shows what it changed ------------------------------------------------------

/// What of the window a turn must leave alone until it has ended: where the
/// arrangement is scrolled and zoomed to, what is selected, which tab is
/// open.
type View = (TimeView, f32, Vec<ClipId>, Option<TrackId>, Option<DeviceId>, DetailTab);

impl Harness {
    fn view(&self) -> View {
        let state = self.app.state();
        let arrangement = &state.arrangement;
        (
            arrangement.time,
            arrangement.scroll_y,
            state.selected_clips.clone(),
            state.selected_track,
            state.selected_device,
            state.detail_tab,
        )
    }

    /// The beats the arrangement shows, from its left edge to its right.
    fn beats_in_view(&self) -> (f64, f64) {
        let arrangement = &self.app.state().arrangement;
        arrangement.time.visible(arrangement.lanes_width)
    }

    /// Adds a clip to `track` from outside the window, as a tool does, and
    /// has the script answer the call `id` with it.
    fn tool_adds_clip(&mut self, script: &Script, id: &str, track: TrackId, name: &str, start: f64) -> ClipId {
        let clip = self.session.lock().edit(|project| project.add_midi_clip(track, name, start, 8.0)).unwrap();
        script.say([answer(id, "create_clip", Ok(&format!("{{\"clip_id\":{},\"note_count\":0}}", clip.0)))]);
        clip
    }
}

fn create_clip(id: &str, track: TrackId, name: &str, start: f64) -> Event {
    call(id, "create_clip", json!({ "track": track.0, "start": start, "length": 8, "name": name }))
}

#[test]
fn when_a_turn_has_ended_the_window_shows_what_it_changed_and_not_before() {
    let options = UiOptions { detail_tab: DetailTab::Mixer, ..Default::default() };
    let (mut harness, ..) = with_three_tracks(options);
    // Enough tracks that a new one lands below what the arrangement shows.
    harness
        .session
        .lock()
        .edit(|project| {
            for number in 4..=14 {
                project.add_track(format!("Track {number}"));
            }
            Ok(())
        })
        .unwrap();
    let script = Script::ready();
    harness.open_assistant(&script);

    harness.tell("add a bass part at bar 51");
    let before = harness.view();
    assert_eq!((before.1, before.5), (0.0, DetailTab::Mixer));
    script.say([started(), call("1", "create_track", json!({ "name": "Bass" }))]);
    harness.idle(2);
    let bass = harness.session.lock().edit(|project| Ok(project.add_track("Bass"))).unwrap();
    script.say([
        answer("1", "create_track", Ok(&format!("{{\"track_id\":{}}}", bass.0))),
        create_clip("2", bass, "Line", 200.0),
    ]);
    harness.idle(2);
    let line = harness.tool_adds_clip(&script, "2", bass, "Line", 200.0);
    script.say([text("Added a bass part at bar 51.")]);
    harness.idle(20);
    assert!(harness.app.assistant().is_working());
    assert_eq!(harness.app.snapshot().project.tracks.len(), 15, "the song has changed under the window");
    assert_eq!(harness.view(), before, "and the view has not moved while the turn is under way");

    script.say([finished()]);
    harness.idle(30);
    let state = harness.app.state();
    assert_eq!(state.selected_clips, [line], "what was made is selected");
    assert_eq!((state.selected_clip, state.selected_track), (Some(line), Some(bass)));
    assert_eq!(state.detail_tab, DetailTab::Clip, "and the tab that shows a clip is open on it");
    let (from, to) = harness.beats_in_view();
    assert!(from <= 200.0 && to >= 208.0, "the timeline was scrolled to the clip: {from} to {to}");
    assert_eq!(state.arrangement.time.pixels_per_beat, before.0.pixels_per_beat, "scrolling was enough: no zoom");
    assert!(state.arrangement.scroll_y > 0.0, "the track list was scrolled down to the new track");
    assert_eq!(harness.undo_depth(), 3, "showing is not an edit: the song, the tracks, and the turn");

    // A turn that changes nothing moves nothing.
    let shown = harness.view();
    harness.tell("what key is it in?");
    script.say([
        started(),
        call("3", "get_project", json!({})),
        answer("3", "get_project", Ok("{}")),
        text("C major."),
        finished(),
    ]);
    harness.idle(30);
    assert_eq!(harness.view(), shown);
}

#[test]
fn the_view_is_left_alone_while_the_user_holds_something_or_is_at_work_elsewhere() {
    let (mut harness, script, tracks, _) = assisted();

    // The user takes hold of a fader while the assistant works, and is
    // still holding it when the turn ends.
    harness.tell("add a clip far along");
    script.say([started(), create_clip("1", tracks[2], "Far", 400.0)]);
    harness.idle(2);
    let far = harness.tool_adds_clip(&script, "1", tracks[2], "Far", 400.0);
    let (grab, pull) = (harness.volume_bar(0, 0.84), harness.volume_bar(0, 0.4));
    harness.drag_start(Modifiers::NONE, grab, pull);
    let held = harness.view();
    script.say([text("Done."), finished()]);
    harness.idle(10);
    assert!(!harness.app.assistant().is_working(), "the turn has ended");
    assert_eq!(harness.view(), held, "the view does not move while something is being dragged");
    harness.drag_end(pull);
    harness.idle(60);
    assert_eq!(harness.view(), held, "and is not sprung on the user when they let go");
    assert!(harness.project().track(tracks[0]).unwrap().volume_db < -10.0, "the drag was theirs to finish");

    // The turn's own line still leads there, when they want to look.
    harness.click_label("Show what changed");
    harness.idle(30);
    assert_eq!(harness.selected_clips(), [far]);
    assert_eq!(harness.app.state().detail_tab, DetailTab::Clip);
    let (from, to) = harness.beats_in_view();
    assert!(from <= 400.0 && to >= 408.0, "{from} to {to}");

    // A click in the arrangement a moment before the turn ends: they are
    // at work there, and are left to it.
    harness.tell("and another");
    script.say([started(), create_clip("2", tracks[2], "Further", 600.0)]);
    harness.idle(2);
    harness.tool_adds_clip(&script, "2", tracks[2], "Further", 600.0);
    harness.click(harness.lane(from + 1.0, 1));
    let there = harness.view();
    assert_eq!(there.3, Some(tracks[1]), "the click selected the second track");
    script.say([finished()]);
    harness.idle(30);
    assert_eq!(harness.view(), there);

    // Having done the same and then waited, they are watching again: the
    // next turn shows what it made.
    harness.tell("and one more");
    script.say([started(), create_clip("3", tracks[2], "Furthest", 800.0)]);
    harness.idle(2);
    let furthest = harness.tool_adds_clip(&script, "3", tracks[2], "Furthest", 800.0);
    harness.click(harness.lane(from + 1.0, 1));
    harness.idle((AT_WORK / FRAME) as usize + 10);
    assert_eq!(harness.view(), there, "waiting alone moves nothing");
    script.say([finished()]);
    harness.idle(30);
    assert_eq!(harness.selected_clips(), [furthest]);
    let (from, to) = harness.beats_in_view();
    assert!(from <= 800.0 && to >= 808.0, "{from} to {to}");
}

#[test]
fn the_line_of_a_call_that_changed_something_leads_to_it() {
    let (mut harness, script, tracks, _) = assisted();
    harness.tell("a line for the second track, reverb on the first, the third quieter");
    script.say([
        started(),
        call("1", "get_project", json!({})),
        answer("1", "get_project", Ok("{}")),
        create_clip("2", tracks[1], "Line", 24.0),
    ]);
    harness.idle(2);
    let line = harness.tool_adds_clip(&script, "2", tracks[1], "Line", 24.0);
    script.say([call("3", "add_device", json!({ "track": tracks[0].0, "uri": "builtin:reverb" }))]);
    harness.idle(2);
    let reverb = harness.session.lock().add_device(ChainRef::Track(tracks[0]), "builtin:reverb", None, None).unwrap();
    script.say([
        answer("3", "add_device", Ok(&format!("{{\"device_id\":{},\"name\":\"Reverb\"}}", reverb.0))),
        call("4", "update_track", json!({ "track": tracks[2].0, "volume_db": -6 })),
    ]);
    harness.idle(2);
    harness.session.lock().edit(|project| project.set_track_volume(tracks[2], -6.0)).unwrap();
    script.say([answer("4", "update_track", Ok("{}")), text("Done."), finished()]);
    harness.idle(30);
    // Shown as the turn ended: everything it made is selected, and the tab
    // and the track are those of the device, which came after the clip.
    assert_eq!(harness.selected_clips(), [line]);
    assert_eq!(harness.app.state().selected_device, Some(reverb));
    assert_eq!(
        (harness.app.state().selected_track, harness.app.state().detail_tab),
        (Some(tracks[0]), DetailTab::Devices)
    );

    let tool = |harness: &Harness, name: &str| {
        let found = harness.items().iter().find_map(|item| match item {
            Item::Tool(line) if line.tool == name => Some(line.clone()),
            _ => None,
        });
        found.unwrap_or_else(|| panic!("no call to {name}"))
    };
    // A call that only read leads nowhere: a click unfolds it, as before.
    harness.click_label("Read the project");
    assert!(tool(&harness, "get_project").open && tool(&harness, "get_project").place.is_none());
    assert_eq!(harness.app.state().selected_track, Some(tracks[0]), "and nothing else happened");

    // The clip's line: the clip, its track, the Clip tab.
    harness.click_label("Added the clip \u{201C}Line\u{201D} to \u{201C}Two\u{201D}");
    assert_eq!(harness.selected_clips(), [line]);
    assert_eq!(
        (harness.app.state().selected_track, harness.app.state().detail_tab),
        (Some(tracks[1]), DetailTab::Clip)
    );
    assert!(!tool(&harness, "create_clip").open, "going there does not unfold the line");
    // The track's: the track, where it is; the tab stays.
    harness.click_label("Changed the volume of \u{201C}Three\u{201D}");
    assert_eq!(
        (harness.app.state().selected_track, harness.app.state().detail_tab),
        (Some(tracks[2]), DetailTab::Clip)
    );
    // The device's: its track, the Devices tab, the device chosen in it.
    harness.click_label("Added reverb to \u{201C}One\u{201D}");
    assert_eq!(harness.app.state().selected_device, Some(reverb));
    assert_eq!(
        (harness.app.state().selected_track, harness.app.state().detail_tab),
        (Some(tracks[0]), DetailTab::Devices)
    );

    // What was sent and answered is still a click away, on the caret.
    harness.click_label("Details of: Added the clip \u{201C}Line\u{201D} to \u{201C}Two\u{201D}");
    assert!(tool(&harness, "create_clip").open);
    harness.move_to(harness.labeled("Added the clip \u{201C}Line\u{201D} to \u{201C}Two\u{201D}"));
    harness.idle(2);
    assert!(harness.hint().starts_with("Show this in the song"), "{}", harness.hint());

    // Taken back, the clip is gone and its line leads nowhere: a click on
    // it folds it away again, and the turn offers nothing to show.
    harness.click_label("Undo");
    harness.idle(3);
    assert!(harness.project().clip(line).is_err());
    assert!(!harness.is_on_screen("Show what changed"), "a turn that was taken back has nothing to show");
    assert!(!harness.is_on_screen("Details of: Added the clip") && !harness.is_on_screen("Details of: Added reverb"));
    assert!(harness.is_on_screen("Details of: Changed the volume"), "the track is still there to go to");
    harness.click_label("Added the clip \u{201C}Line\u{201D} to \u{201C}Two\u{201D}");
    assert!(!tool(&harness, "create_clip").open);
    assert!(harness.selected_clips().is_empty());
    // Brought back, it is the same clip, and the line leads to it again.
    harness.click_label("Redo");
    harness.idle(3);
    harness.click_label("Show what changed");
    assert_eq!(harness.selected_clips(), [line]);
    assert_eq!(harness.undo_depth(), 2, "going to see is never an edit");
}

#[test]
fn a_turn_whose_calls_do_not_say_is_shown_by_what_differs_in_the_song() {
    let (mut harness, script, tracks, _) = assisted();
    assert_eq!(harness.app.state().selected_track, Some(tracks[0]));
    harness.tell("polish it");
    // A tool the panel has no words for, and whose answer names nothing.
    script.say([started(), call("1", "polish_mix", json!({ "amount": 3 }))]);
    harness.idle(2);
    harness.session.lock().edit(|project| project.set_track_volume(tracks[2], -9.0)).unwrap();
    script.say([answer("1", "polish_mix", Ok("polished")), finished()]);
    harness.idle(10);
    let Some(Item::End(end)) = harness.items().last() else { panic!("the turn has ended") };
    assert_eq!(end.places, [Place::Track { id: tracks[2].0, name: "Three".into() }]);
    assert_eq!(harness.app.state().selected_track, Some(tracks[2]), "the track that differs was gone to");
    assert!(harness.is_on_screen("Show what changed") && !harness.is_on_screen("Details of:"));
}

// ---- 5: replies read and copy like text ----------------------------------------------

impl Harness {
    /// Where the passage of the conversation that reads `words` is.
    fn passage(&self, words: &str) -> Rect {
        let found = self.labels.iter().find(|(text, _)| text == words);
        found.unwrap_or_else(|| panic!("no passage reads {words:?}: {:?}", self.labels)).1
    }

    /// The labels of what is on show in the assistant's panel.
    fn said_in_the_panel(&self) -> Vec<&str> {
        let left = SCREEN.x - self.app.assistant().width();
        self.labels.iter().filter(|(_, rect)| rect.left() >= left).map(|(text, _)| text.as_str()).collect()
    }
}

#[test]
fn a_reply_is_selected_with_the_mouse_and_copied() {
    let (mut harness, script, _, clips) = assisted();
    harness.tell("busier hats");
    script.say([
        started(),
        text("The **hats** now run in `sixteenths`.\n\nThe pad is down a little.\n\n- Kick on one\n- Snare on two"),
        finished(),
    ]);
    harness.idle(3);
    // Set as it reads: the marks are gone.
    let (first, second) =
        (harness.passage("The hats now run in sixteenths."), harness.passage("The pad is down a little."));
    assert!(second.top() > first.bottom(), "a paragraph under the other");

    // A clip is selected in the arrangement, as it would be.
    harness.click(harness.lane(9.0, 0));
    assert_eq!(harness.selected_clips(), [clips[0]]);

    // Dragged across from the start of one paragraph to the end of the
    // next, and copied.
    harness.drag(first.left_center() + vec2(1.0, 0.0), second.right_center() - vec2(1.0, 0.0));
    harness.copy();
    assert_eq!(harness.clipboard.as_deref(), Some("The hats now run in sixteenths.\n\nThe pad is down a little."));
    assert!(harness.app.state().clipboard.is_none(), "Ctrl+C meant the words, not the clip that is selected");
    assert_eq!(harness.selected_clips(), [clips[0]], "and the selection in the song is as it was");

    // Part of a line; and the items of a list come out line under line.
    harness.drag(first.left_center() + vec2(1.0, 0.0), first.center());
    harness.copy();
    let part = harness.clipboard.clone().unwrap();
    assert!(part.starts_with("The hats") && part.len() < "The hats now run in sixteenths.".len(), "{part:?}");
    let (kick, snare) = (harness.passage("Kick on one"), harness.passage("Snare on two"));
    harness.drag(kick.left_center() + vec2(1.0, 0.0), snare.right_center() - vec2(1.0, 0.0));
    harness.copy();
    assert_eq!(harness.clipboard.as_deref(), Some("Kick on one\nSnare on two"));

    // A double-click takes a word.
    harness.settle();
    harness.double_click(first.left_center() + vec2(4.0, 0.0));
    harness.copy();
    assert_eq!(harness.clipboard.as_deref(), Some("The"));

    // The user's own message is text too.
    let message = harness.passage("You: busier hats");
    harness.settle();
    harness.double_click(pos2(message.left() + 16.0, message.bottom() - 16.0));
    harness.copy();
    assert_eq!(harness.clipboard.as_deref(), Some("busier"));

    // A click back in the song lets go of the words, and Ctrl+C is the
    // clip's again.
    harness.settle();
    harness.click(harness.lane(9.0, 0));
    harness.copy();
    assert_eq!(harness.clipboard.as_deref(), Some("1 clip from Trill"));
    assert!(harness.app.state().clipboard.is_some());
    assert_eq!(harness.undo_depth(), 1, "none of it touched the song");
}

#[test]
fn the_menu_of_a_reply_copies_all_of_it_as_it_reads() {
    let (mut harness, script, ..) = assisted();
    harness.tell("busier hats");
    let said =
        "## Done\n\nThe **hats** now run in `sixteenths`.\n\n1. Kick on one\n2. Snare on two\n\n```\nx . x .\n```";
    script.say([started(), text(said), finished()]);
    harness.idle(3);

    harness.right_click(harness.passage("The hats now run in sixteenths.").center());
    harness.click_label("Copy reply");
    assert_eq!(
        harness.clipboard.as_deref(),
        Some("Done\n\nThe hats now run in sixteenths.\n\n1. Kick on one\n2. Snare on two\n\nx . x ."),
        "the whole reply, without the marks that set it"
    );

    // The user's message: copied as it was typed, or put back to send again.
    let message = harness.passage("You: busier hats");
    harness.right_click(pos2(message.left() + 16.0, message.bottom() - 16.0));
    harness.click_label("Copy message");
    assert_eq!(harness.clipboard.as_deref(), Some("busier hats"));
    harness.right_click(pos2(message.right() - 30.0, message.top() + 6.0));
    harness.click_label("Put back in the message field");
    assert_eq!(harness.app.assistant().draft(), "busier hats");
}

#[test]
fn markdown_that_arrives_in_pieces_is_shown_without_its_marks_and_set_once() {
    let (mut harness, script, ..) = assisted();
    harness.tell("busier hats");
    script.say([started()]);
    harness.idle(2);
    // Cut where a real reply is cut: inside a mark, a word, a fence.
    let pieces = [
        "I **dou",
        "bled** the `ha",
        "ts`.\n\n- Ki",
        "ck on *o",
        "ne*\n- Sna",
        "re\n\n``",
        "`\nAm | F\n`",
        "``\nDo",
        "ne.",
    ];
    let mut set = harness.app.assistant().blocks_set();
    let mut shown_before = 0;
    for piece in pieces {
        script.say([text(piece)]);
        harness.idle(3);
        let panel = harness.said_in_the_panel();
        assert!(
            !panel.iter().any(|label| label.contains(['*', '`'])),
            "a mark was shown as it arrived, after {piece:?}: {panel:?}"
        );
        let now = harness.app.assistant().blocks_set();
        assert!(now - set <= 2, "{} blocks were set for {piece:?}: only what changed should be", now - set);
        set = now;
        // What is on show only ever grows.
        let shown: usize = panel.iter().map(|label| label.len()).sum();
        assert!(shown >= shown_before, "something was taken back after {piece:?}");
        shown_before = shown;
    }
    for words in ["I doubled the hats.", "Kick on one", "Snare", "Am | F", "Done."] {
        assert!(harness.shows(words), "{words:?} is not on show: {:?}", harness.said_in_the_panel());
    }
    // Held back until it is known what they are: half a mark, the start of
    // a list, of a fence.
    script.say([text("\n\n1")]);
    harness.idle(2);
    assert!(!harness.shows("1"));
    script.say([text(". Then **")]);
    harness.idle(2);
    assert!(harness.shows("Then") && !harness.said_in_the_panel().iter().any(|label| label.contains('*')));

    // Nothing is set again for being looked at.
    let set = harness.app.assistant().blocks_set();
    harness.idle(60);
    assert_eq!(harness.app.assistant().blocks_set(), set);

    // Nor while the panel is closed, however much arrives.
    harness.key(CTRL_SHIFT, Key::A);
    assert!(!harness.app.assistant().is_open());
    script.say([text("loud"), text("** and more"), text(" words, and more still.")]);
    harness.idle(20);
    assert_eq!(harness.app.assistant().blocks_set(), set, "a closed panel sets nothing");
    assert!(harness.reply().ends_with("more still."), "though the reply was taken in");
    harness.key(CTRL_SHIFT, Key::A);
    harness.idle(2);
    assert!(harness.shows("Then loud and more words, and more still."));
    assert_eq!(harness.app.assistant().blocks_set(), set + 1, "opened again, the one block that grew is set");
    script.say([finished()]);
    harness.idle(2);
}

// ---- 6: the model is chosen in the panel ---------------------------------------------

#[test]
fn the_model_is_chosen_in_the_panel_and_reaches_the_back_end() {
    let (mut harness, script, ..) = assisted();
    assert!(harness.shows("Model: Default"), "the CLI's own choice until another is made");
    assert_eq!(harness.app.assistant().model_choice(), None);
    harness.tell("hello");
    assert_eq!(script.state().models, [None], "no model is asked for: the choice is the CLI's");

    // While the assistant is answering, the model is not to be changed.
    harness.click_label("Model: Default");
    harness.idle(3);
    assert!(!harness.shows("sonnet"), "the list does not open");
    assert!(harness.app.assistant_mut().choose_model(Some("sonnet")).is_err());
    script.say([started(), text("Hello."), finished()]);
    harness.idle(3);

    // The list: the CLI's own choice, the names it takes, a field for any other.
    harness.click_label("Model: Default");
    harness.idle(3);
    assert!(harness.is_on("Default"));
    for alias in trill_assistant::MODEL_ALIASES {
        assert!(harness.shows(alias) && !harness.is_on(alias), "{alias}");
    }
    assert!(harness.shows("Another model name"));
    assert!(harness.shows("Answering as test-model"), "what the CLI said it is answering as");
    // The list opens inside the panel, however wide what is in it would be.
    let panel = SCREEN.x - harness.app.assistant().width()..=SCREEN.x;
    let (first, last) = (harness.passage("Default"), harness.passage("Answering as test-model"));
    assert!(panel.contains(&first.left()) && panel.contains(&first.right()), "{first:?} in {panel:?}");
    assert!(
        first.top() < last.top() && last.bottom() <= harness.passage("Model: Default").top() + 1.0,
        "above the chip"
    );
    // Nothing in the window, the open list included, promises anything.
    for (label, _) in &harness.labels {
        let lowered = label.to_lowercase();
        for promise in ["included", "unlimited", "your plan", "sign in", "log in"] {
            assert!(!lowered.contains(promise), "the panel says {label:?}");
        }
    }
    harness.click_label("sonnet");
    harness.idle(3);
    assert_eq!(harness.app.assistant().model_choice(), Some("sonnet"));
    assert!(harness.shows("Model: sonnet") && !harness.shows("Default"), "chosen, and the list is put away");
    let carries =
        "The model \u{201C}sonnet\u{201D} answers from the next message on. The conversation carries on with it.";
    assert!(
        harness.shows(carries),
        "what happens to the conversation is said in it: {:?}",
        harness.said_in_the_panel()
    );

    // The next message is the same conversation, opened again for that model.
    harness.tell("again");
    assert_eq!(script.state().opened, [None, Some(conversation_id('a'))], "resumed, not begun anew");
    assert_eq!(script.state().models, [None, Some("sonnet".to_owned())]);
    let start = TurnStart { session_id: conversation_id('a'), model: "sonnet-as-named".into(), api_key_source: None };
    script.say([Event::Started(start), finished()]);
    harness.idle(3);
    assert_eq!(harness.app.assistant().conversation(), Some(&conversation_id('a')));

    // A name of the user's own, typed.
    harness.click_label("Model: sonnet");
    harness.idle(3);
    assert!(harness.is_on("sonnet") && harness.shows("Answering as sonnet-as-named"));
    harness.click_label("Another model name");
    harness.type_text("claude-haiku-5-5");
    harness.key(Modifiers::NONE, Key::Enter);
    harness.idle(3);
    assert_eq!(harness.app.assistant().model_choice(), Some("claude-haiku-5-5"));
    assert!(harness.shows("Model: claude-haiku-5-5"));
    // Changed twice before anything was sent: said once, as it now stands.
    let notes = harness.items().iter().filter(|item| matches!(item, Item::Note { .. })).count();
    assert_eq!(notes, 2, "one for each change that a message followed, and for this one: {:?}", harness.items());
    assert!(harness.shows(
        "The model \u{201C}claude-haiku-5-5\u{201D} answers from the next message on. The conversation carries on with it."
    ));
    // Back to what last answered: there is no change to speak of.
    harness.click_label("Model: claude-haiku-5-5");
    harness.idle(3);
    assert!(harness.is_on("claude-haiku-5-5"), "the name in use is in the list");
    harness.click_label("sonnet");
    harness.idle(3);
    assert_eq!(harness.items().iter().filter(|item| matches!(item, Item::Note { .. })).count(), 1);

    // What could not be a name is refused where it was typed, with the
    // field still open to put it right and the choice left as it was.
    harness.click_label("Model: sonnet");
    harness.idle(3);
    harness.click_label("Another model name");
    harness.type_text("two words");
    harness.key(Modifiers::NONE, Key::Enter);
    harness.idle(3);
    assert_eq!(harness.app.assistant().model_choice(), Some("sonnet"));
    assert!(
        harness.shows(
            "\u{201C}two words\u{201D} cannot be the name of a model: it has a space in it, or begins with a dash."
        ),
        "{:?}",
        harness.labels
    );
    assert!(harness.app.assistant_mut().choose_model(Some("--resume=x")).is_err());

    // And back to the CLI's own choice, from the list that is still open.
    harness.click_label("Default");
    harness.idle(3);
    assert_eq!(harness.app.assistant().model_choice(), None);
    harness.tell("once more");
    assert_eq!(script.state().models.last(), Some(&None));
    assert_eq!(script.state().opened.len(), 3, "each change opened the conversation again, by its id");
    assert_eq!(script.state().opened[2], Some(conversation_id('a')));
}

#[test]
fn the_chosen_model_is_remembered_from_one_session_to_the_next() {
    let home = tempfile::tempdir().unwrap();
    let start = || {
        let storage = trill_runtime::Storage::At { settings: home.path().join("settings.json"), library_index: None };
        Arc::new(Runtime::start(RuntimeOptions { storage, ..RuntimeOptions::hermetic() }).unwrap())
    };
    let kept = || std::fs::read_to_string(home.path().join("assistant.json")).unwrap();

    let mut harness = Harness::on(start(), UiOptions::default(), |_| {});
    harness.open_assistant(&Script::ready());
    harness.click_label("Model: Default");
    harness.click_label("opus");
    harness.idle(3);
    drop(harness);
    assert!(kept().contains("\"model\": \"opus\""), "{}", kept());

    // The next session starts with it, and asks for it.
    let mut harness = Harness::on(start(), UiOptions::default(), |_| {});
    let script = Script::ready();
    harness.app.assistant_mut().use_script(script.clone());
    assert_eq!(harness.app.assistant().model_choice(), Some("opus"));
    harness.idle(3);
    assert!(harness.shows("Model: opus"));
    harness.tell("hello");
    assert_eq!(script.state().models, [Some("opus".to_owned())]);
    assert!(!harness.items().iter().any(|item| matches!(item, Item::Note { .. })), "nothing changed: nothing to say");
    drop(harness);

    // A run told on the command line which model to use starts with that
    // one, and what is remembered stays as it was chosen.
    let mut options = UiOptions::default();
    options.assistant.model = Some("haiku".to_owned());
    let harness = Harness::on(start(), options, |_| {});
    assert_eq!(harness.app.assistant().model_choice(), Some("haiku"));
    drop(harness);
    assert!(kept().contains("\"model\": \"opus\""), "{}", kept());

    // Back to the CLI's own choice: the name goes from the file.
    let mut harness = Harness::on(start(), UiOptions::default(), |_| {});
    harness.app.assistant_mut().use_script(Script::ready());
    harness.idle(3);
    harness.click_label("Model: opus");
    harness.click_label("Default");
    harness.idle(3);
    drop(harness);
    assert!(!kept().contains("opus") && !kept().contains("\"model\""), "{}", kept());
}

#[test]
fn a_model_that_cannot_be_used_is_said_in_the_clis_words_and_another_can_be_chosen() {
    let (mut harness, script, ..) = assisted();
    harness.app.assistant_mut().choose_model(Some("not-a-model-name")).unwrap();
    harness.idle(2);
    harness.tell("hello");
    assert_eq!(script.state().models, [Some("not-a-model-name".to_owned())], "the name goes to the CLI as it is");
    let said = "There's an issue with the selected model (not-a-model-name). It may not exist or you may not have \
                access to it. Run --model to pick a different model.";
    let reason = FailureReason::ModelUnavailable { message: said.into() };
    script.say([started(), Event::Failed(reason.clone())]);
    harness.idle(3);
    assert!(harness.is_on_screen(&reason.to_string()), "{:?}", harness.said_in_the_panel());
    assert!(
        reason.to_string().contains(said) && reason.to_string().contains("under the assistant\u{2019}s message field")
    );
    assert_eq!(script.state().checks, 1, "nothing is wrong with the CLI: it is not looked for again");
    assert!(harness.is_on_screen("Send") && harness.is_on_screen("Try again"));

    // Another model, and the same message again.
    harness.click_label("Model: not-a-model-name");
    harness.click_label("haiku");
    harness.idle(3);
    harness.click_label("Try again");
    let state = script.state();
    assert_eq!(state.sent.last().unwrap().message, "hello");
    assert_eq!(state.models.last(), Some(&Some("haiku".to_owned())));
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
    // Which examples there are goes by what is selected.
    script.say([started(), finished()]);
    harness.idle(2);
    harness.click_label("New conversation");
    assert!(!harness.is_on_screen("Make the selected clip busier."), "no clip is selected");
    harness.click(harness.lane(9.0, 0));
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
    assert!(harness.is_on_screen(
        "The assistant stopped at the limit of 80 steps for one request. It can carry on from where it is; the \
         limit is there to catch an assistant going round in circles."
    ));
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
    // Long enough for a machine that is busy with something else.
    let deadline = Instant::now() + Duration::from_secs(30);
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
fn a_conversation_goes_along_to_a_project_the_assistant_itself_opens() {
    let home = tempfile::tempdir().unwrap();
    let storage = trill_runtime::Storage::At { settings: home.path().join("settings.json"), library_index: None };
    let runtime = Arc::new(Runtime::start(RuntimeOptions { storage, ..RuntimeOptions::hermetic() }).unwrap());
    let (night, day) = (home.path().join("night.trill"), home.path().join("day.trill"));
    let mut ids = None;
    let mut harness = Harness::on(runtime, UiOptions::default(), |session| {
        // Two songs with the same ids in them, as any two songs have.
        ids = Some(three_tracks(session));
        session.save(&day).unwrap();
        session.save(&night).unwrap();
    });
    let (tracks, clips) = ids.unwrap();
    let script = Script::ready();
    harness.open_assistant(&script);
    let kept = |harness: &mut Harness| {
        harness.app.assistant_mut().store_mut().flush();
        std::fs::read_to_string(home.path().join("assistant.json")).unwrap()
    };

    // The first clip is selected when the message goes.
    harness.click(harness.lane(9.0, 0));
    harness.tell("thin out the first clip, then open my other song and add a part to it");
    script.say([started(), call("1", "update_clip", json!({ "clip": clips[0].0, "muted": true }))]);
    harness.idle(2);
    script.say([
        answer("1", "update_clip", Ok("{}")),
        call("2", "load_project", json!({ "path": day.display().to_string() })),
    ]);
    harness.idle(2);
    let update = |harness: &Harness| {
        let found = harness.items().iter().find_map(|item| match item {
            Item::Tool(line) if line.tool == "update_clip" => Some(line.place.clone()),
            _ => None,
        });
        found.expect("the call is in the conversation")
    };
    assert!(update(&harness).is_some(), "the line of the first call leads to its clip");
    assert!(kept(&mut harness).contains("night.trill"));
    let generation = harness.app.state().project_generation;
    assert_eq!(harness.selected_clips(), [clips[0]]);

    // The tool opens the other song, as the server does it: in the
    // session, with no word to the window.
    harness.session.lock().load(&day).unwrap();
    script
        .say([answer("2", "load_project", Ok("{ \"name\": \"Untitled\" }")), create_clip("3", tracks[1], "Part", 4.0)]);
    harness.idle(3);
    assert!(harness.app.assistant().is_working(), "the turn carries on");
    assert_eq!(script.state().cancels, 0, "nothing was stopped: the assistant opened it itself");
    assert_eq!(harness.items().len(), 4, "and the conversation is all there");
    assert_eq!(harness.app.state().project_generation, generation + 1, "the window started over for the new song");
    assert!(harness.selected_clips().is_empty(), "with nothing of the old one selected");
    assert_eq!(update(&harness), None, "the line no longer leads: the same id is another song's clip now");
    let record = kept(&mut harness);
    assert!(
        record.contains("day.trill") && !record.contains("night.trill"),
        "the conversation is the new song's: {record}"
    );

    // What the turn makes from here on is shown in the song it is made in.
    let part = harness.tool_adds_clip(&script, "3", tracks[1], "Part", 4.0);
    script.say([text("Opened it and added a part."), finished()]);
    harness.idle(30);
    assert_eq!(harness.selected_clips(), [part]);
    assert_eq!(harness.app.assistant().conversation(), Some(&conversation_id('a')));
    assert_eq!(script.state().opened, [None], "one conversation throughout");

    // The song that was left has no conversation; the one gone to has it.
    harness.session.lock().load(&night).unwrap();
    harness.app.state_mut().reset_for_new_project();
    harness.idle(3);
    assert!(harness.items().is_empty());
    assert_eq!(harness.app.assistant().conversation(), None);
    harness.session.lock().load(&day).unwrap();
    harness.app.state_mut().reset_for_new_project();
    harness.idle(2);
    wait_for_the_conversation(&mut harness);
    assert_eq!(harness.app.assistant().conversation(), Some(&conversation_id('a')));
    assert!(harness.is_on_screen("Opened it and added a part."));
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

// ---- a long request, a plan, the web, and what is chosen for them -----------------------

fn progress(steps: u32, cost: Option<f64>, costed: u32) -> Event {
    Event::Progress(Progress { steps, estimated_cost_usd: cost, costed_steps: costed })
}

fn planned(id: &str, title: &str, doing: Option<&str>) -> Event {
    Event::Plan(PlanChange::Added { id: id.to_owned(), title: title.to_owned(), doing: doing.map(str::to_owned) })
}

fn stands(id: &str, status: StepStatus) -> Event {
    Event::Plan(PlanChange::Changed { id: id.to_owned(), status: Some(status), title: None, doing: None })
}

fn looks_up(id: &str, lookup: WebLookup) -> Event {
    Event::WebStarted { id: id.to_owned(), lookup }
}

fn found(id: &str, result: Result<&str, &str>) -> Event {
    Event::WebFinished { id: id.to_owned(), result: result.map(str::to_owned).map_err(str::to_owned) }
}

impl Harness {
    /// Whether something on screen is labeled with words that begin so.
    fn shows_start(&self, start: &str) -> bool {
        self.labels.iter().any(|(text, _)| text.starts_with(start))
    }

    /// Plays `steps` steps of a long request from `from` on: each a round of
    /// the model's with a tool call in it, with the back end saying how far
    /// it has come, starting another run of the CLI every hundred steps and
    /// learning then what the run before cost (1.25 each). Every tenth step
    /// changes the song, from outside the window, as the MCP server does.
    fn work_through(&mut self, script: &Script, track: TrackId, from: u32, steps: u32) {
        for step in from..from + steps {
            let runs_done = (step - 1) / 100;
            let cost = (runs_done > 0).then(|| f64::from(runs_done) * 1.25);
            if step > 1 && step % 100 == 1 {
                script.say([progress(step - 1, cost, step - 1), started()]);
            }
            let id = format!("step-{step}");
            script.say([
                progress(step, cost, runs_done * 100),
                call(&id, "update_track", json!({ "track": track.0, "volume_db": -1 })),
            ]);
            if step % 10 == 0 {
                self.idle(1);
                let volume = -(step as f32) / 20.0;
                self.session.lock().edit(|project| project.set_track_volume(track, volume)).unwrap();
            }
            script.say([answer(&id, "update_track", Ok("{}"))]);
        }
        self.idle(2);
    }
}

#[test]
fn a_request_four_times_as_long_as_a_message_used_to_be_allowed_runs_to_its_end_as_one_undo_step() {
    let (mut harness, script, tracks, _) = assisted();
    let before = harness.project();
    let depth = harness.undo_depth();

    harness.tell("write a whole song");
    script.say([started()]);
    // Eighty steps is where a message used to stop and offer Continue.
    harness.work_through(&script, tracks[0], 1, 150);
    assert!(harness.app.assistant().is_working(), "well past the old limit, and still at work");
    assert!(
        harness.shows("Working\u{2026} \u{00B7} Step 150 of 1,000 \u{00B7} est. $1.25 to step 100"),
        "how far it has come and what the first hundred steps cost: {:?}",
        harness.said_in_the_panel()
    );
    assert!(harness.is_on_screen("Stop") && !harness.shows("Continue"), "there is nothing to click, and Stop is there");

    harness.work_through(&script, tracks[0], 151, 170);
    let summary = TurnSummary {
        turns: 320,
        duration: Duration::from_secs(31 * 60 + 4),
        usage: TokenUsage::default(),
        estimated_cost_usd: Some(4.1),
    };
    script.say([text("The song is written."), Event::Finished(summary)]);
    harness.idle(3);

    // It ran to its end by itself: one message sent, one conversation, no
    // Stop, no Continue.
    let state = script.state();
    assert_eq!((state.sent.len(), state.opened.len(), state.cancels), (1, 1, 0));
    assert_eq!(state.sent[0].message, "write a whole song");
    drop(state);
    assert_eq!(harness.ending(), (Ending::Finished { seconds: 1864.0, steps: 320, estimated_cost: Some(4.1) }, true));
    assert!(
        harness.is_on_screen("Changed the song \u{00B7} 31 min 4 s \u{00B7} 320 steps \u{00B7} est. $4.10"),
        "{:?}",
        harness.said_in_the_panel()
    );
    assert_eq!(harness.items().iter().filter(|item| matches!(item, Item::Tool(_))).count(), 320);

    // And all of it, thirty-two changes to the song over four runs of the
    // CLI, is one step: one Undo takes it back.
    assert_ne!(harness.project(), before);
    assert_eq!(harness.undo_depth(), depth + 1);
    harness.click_label("Undo");
    assert_eq!(harness.project(), before);
}

#[test]
fn stop_ends_a_long_request_at_once_with_what_it_did_as_one_step_and_what_it_took() {
    let (mut harness, script, tracks, _) = assisted();
    let before = harness.project();
    let depth = harness.undo_depth();

    harness.tell("write a whole song");
    script.say([started()]);
    harness.work_through(&script, tracks[0], 1, 130);
    script.say([progress(131, Some(1.25), 100), call("render", "render", json!({}))]);
    harness.idle(2);
    assert!(harness.shows_start("Working\u{2026} \u{00B7} Step 131 of 1,000"));

    harness.click_last_label("Stop");
    assert_eq!(script.state().cancels, 1);
    harness.idle(2);
    assert!(harness.shows_start("Stopping\u{2026}"));
    script.say([Event::Cancelled]);
    harness.idle(3);

    assert!(!harness.app.assistant().is_working());
    let Some(Item::End(end)) = harness.items().last() else { panic!("the turn is closed") };
    assert_eq!((&end.ending, end.changed), (&Ending::Stopped, true));
    // What it had taken when it was stopped, as far as the back end said:
    // the estimate is the CLI's for the run that had ended.
    let spent = end.spent.clone().expect("a turn that had got going says what it took");
    assert_eq!((spent.steps, spent.estimated_cost, spent.costed_steps), (131, Some(1.25), 100));
    assert!(
        harness.labels.iter().any(|(label, _)| label.starts_with("Stopped \u{00B7} Changed the song \u{00B7} ")
            && label.ends_with(" \u{00B7} 131 steps \u{00B7} est. $1.25 to step 100")),
        "{:?}",
        harness.said_in_the_panel()
    );
    // Nothing carries on behind the user's back.
    harness.idle(10);
    assert_eq!(script.state().sent.len(), 1);

    assert_eq!(harness.undo_depth(), depth + 1, "thirteen changes, one step");
    harness.click_label("Undo");
    assert_eq!(harness.project(), before);

    // The conversation goes on from there.
    harness.tell("carry on from the bass");
    assert_eq!(script.state().sent.len(), 2);
    assert_eq!(script.state().opened.len(), 1, "in the conversation that was stopped");
}

#[test]
fn a_request_that_fails_after_it_has_done_work_offers_to_carry_on_and_not_to_start_again() {
    let (mut harness, script, tracks, _) = assisted();
    harness.tell("write a whole song");
    script.say([started()]);
    harness.work_through(&script, tracks[0], 1, 12);
    let reason = FailureReason::UsageLimitReached {
        message: "You've hit your session limit \u{00B7} resets 3:00pm".into(),
        resets_at: None,
    };
    script.say([Event::Failed(reason.clone())]);
    harness.idle(3);

    assert_eq!(harness.ending(), (Ending::Failed { text: reason.to_string(), offer: Offer::Continue }, true));
    assert!(harness.shows("Continue") && !harness.shows("Try again"), "the request must not be made a second time");
    assert!(harness.shows("Undo"), "what it did is one step all the same");
    harness.click_label("Continue");
    assert_eq!(script.state().sent[1].message, "Continue.");
}

#[test]
fn the_plan_is_a_checklist_that_fills_in_as_the_work_proceeds() {
    let (mut harness, script, tracks, _) = assisted();
    assert!(!harness.shows_start("Plan:"), "no plan, no checklist");

    harness.tell("write a song");
    script.say([
        started(),
        text("Here is the plan."),
        planned("1", "Set tempo and key", Some("Setting tempo and key")),
        planned("2", "Write the drums", Some("Writing the drums")),
        planned("3", "Render and check", None),
    ]);
    harness.idle(3);
    assert!(harness.shows("Plan: 0 of 3 done"));
    for step in ["Set tempo and key: to do", "Write the drums: to do", "Render and check: to do"] {
        assert!(harness.shows(step), "{step}: {:?}", harness.said_in_the_panel());
    }
    assert!(
        !harness.items().iter().any(|item| matches!(item, Item::Tool(_))),
        "the plan is the checklist, not lines in the conversation"
    );
    // It stands between the conversation and the message field.
    let (plan, field) = (harness.passage("Plan: 0 of 3 done"), harness.app.assistant().field());
    assert!(harness.passage("Here is the plan.").bottom() <= plan.top() && plan.bottom() < field.top());

    // A step in hand reads as what is being done; done, it is ticked.
    script.say([stands("1", StepStatus::InProgress)]);
    harness.idle(2);
    assert!(harness.shows("Setting tempo and key: in hand") && harness.shows("Plan: 0 of 3 done"));
    script.say([
        call("a", "set_project", json!({ "tempo": 122 })),
        answer("a", "set_project", Ok("{}")),
        stands("1", StepStatus::Done),
        stands("2", StepStatus::InProgress),
    ]);
    harness.idle(2);
    assert!(harness.shows("Plan: 1 of 3 done"));
    assert!(harness.shows("Set tempo and key: done") && harness.shows("Writing the drums: in hand"));

    // Steps are worded anew and dropped as the assistant changes its mind,
    // and the list is what the CLI says it is when the assistant reads it.
    let reworded = PlanChange::Changed {
        id: "3".into(),
        status: None,
        title: Some("Render and read the mix".into()),
        doing: None,
    };
    script.say([Event::Plan(reworded)]);
    harness.idle(2);
    assert!(harness.shows("Render and read the mix: to do") && !harness.shows_start("Render and check"));
    let step = |id: &str, title: &str, status| PlanStep { id: id.into(), title: title.into(), doing: None, status };
    script.say([Event::Plan(PlanChange::Listed(vec![
        step("1", "Set tempo and key", StepStatus::Done),
        step("2", "Write the drums", StepStatus::InProgress),
        step("4", "Write the bass", StepStatus::Pending),
    ]))]);
    harness.idle(2);
    assert!(harness.shows("Plan: 1 of 3 done") && harness.shows("Write the bass: to do"));
    assert!(harness.shows("Writing the drums: in hand"), "how a step reads while it is done is kept");
    assert!(!harness.shows_start("Render and read the mix"), "a step the CLI no longer lists is gone");
    script.say([Event::Plan(PlanChange::Removed { id: "4".into() })]);
    harness.idle(2);
    assert!(harness.shows("Plan: 1 of 2 done") && !harness.shows_start("Write the bass"));

    // Folded, it is its heading; unfolded, the steps are back.
    harness.click_label("Plan: 1 of 2 done");
    harness.idle(2);
    assert!(harness.shows("Plan: 1 of 2 done") && !harness.shows("Set tempo and key: done"));
    harness.click_label("Plan: 1 of 2 done");
    harness.idle(2);
    assert!(harness.shows("Set tempo and key: done"));

    // Stopped half way, the plan stays as it stood, with nothing in hand.
    harness.click_last_label("Stop");
    script.say([Event::Cancelled]);
    harness.idle(3);
    assert!(harness.shows("Plan: 1 of 2 done") && harness.shows("Writing the drums: begun"));
    // The next request may carry it on: it is kept.
    harness.tell("carry on");
    harness.idle(2);
    assert!(harness.shows("Plan: 1 of 2 done"));
    script.say([started(), stands("2", StepStatus::Done), text("All done."), finished()]);
    harness.idle(3);
    assert!(harness.shows("Plan: 2 of 2 done") && harness.shows("Write the drums: done"), "read once it is done");

    // A plan that was carried out belongs to its request: the next one
    // starts without it.
    harness.session.lock().edit(|project| project.set_track_volume(tracks[0], -1.0)).unwrap();
    harness.tell("thanks");
    harness.idle(2);
    assert!(!harness.shows_start("Plan:"));
}

#[test]
fn the_web_is_on_shown_and_listed_in_use_and_switched_off_it_is_not_offered_at_all() {
    let (mut harness, script, ..) = assisted();
    assert!(harness.app.assistant().web() && harness.is_on("Web research"), "on until it is switched off");

    harness.tell("what is a tresillo?");
    assert!(script.state().setups[0].web);
    script.say([started(), looks_up("w1", WebLookup::Search { query: "tresillo rhythm pattern".into() })]);
    harness.idle(2);
    // Every search and every page is a line, by its query or its address,
    // from the moment it is asked for.
    assert!(harness.shows("Searching the web for \u{201C}tresillo rhythm pattern\u{201D}\u{2026}"));
    let question = "How is it counted?".to_owned();
    script.say([
        found("w1", Ok("Web search results for query: \"tresillo rhythm pattern\"")),
        looks_up(
            "w2",
            WebLookup::Page { url: "https://example.com/rhythm/tresillo".into(), question: question.clone() },
        ),
        found("w2", Ok("Three notes over eight sixteenths: 3 + 3 + 2.")),
        looks_up("w3", WebLookup::Page { url: "https://example.org/missing/".into(), question }),
        found("w3", Err("Request failed with status code 404")),
    ]);
    harness.idle(2);
    assert!(harness.shows("Searched the web for \u{201C}tresillo rhythm pattern\u{201D}"));
    assert!(harness.shows("Read example.com/rhythm/tresillo"));
    assert!(harness.shows("Reading example.org/missing: failed"), "{:?}", harness.said_in_the_panel());
    // What was asked of the page and what came back are a click away.
    harness.click_label("Read example.com/rhythm/tresillo");
    harness.idle(2);
    let Some(Item::Tool(page)) = harness.items().iter().find(|item| matches!(item, Item::Tool(line) if line.open))
    else {
        panic!("the line unfolds")
    };
    assert_eq!(page.tool, "WebFetch", "by the name the CLI has for it");
    assert_eq!(page.input, "https://example.com/rhythm/tresillo\n\nHow is it counted?");
    assert_eq!(page.output, "Three notes over eight sixteenths: 3 + 3 + 2.");

    // While the assistant is answering, the switch stays as it is.
    harness.click_label("Web research");
    harness.idle(2);
    assert!(harness.app.assistant().web(), "a turn is answered with what it began with");
    script.say([text("Three, three, two."), finished()]);
    harness.idle(3);

    // Switched off: it shows, and the conversation is opened again, by its
    // id, without the web.
    harness.click_label("Web research");
    harness.idle(2);
    assert!(!harness.app.assistant().web() && !harness.is_on("Web research"));
    harness.tell("and a habanera?");
    let state = script.state();
    assert_eq!(state.opened, [None, Some(conversation_id('a'))], "the same conversation, opened again");
    assert_eq!(state.setups.iter().map(|setup| setup.web).collect::<Vec<_>>(), [true, false]);
}

#[test]
fn the_thinking_effort_is_chosen_beside_the_model_and_reaches_the_back_end() {
    use trill_assistant::Effort;
    let (mut harness, script, ..) = assisted();
    assert!(harness.shows("Effort: Default"), "the CLI's own until one is chosen");
    // Beside the model chooser, on its line.
    let (model, effort) = (harness.passage("Model: Default"), harness.passage("Effort: Default"));
    assert!(model.right() <= effort.left() && (model.center().y - effort.center().y).abs() < 1.0);
    harness.tell("hello");
    assert_eq!(script.state().setups[0].effort, None, "no effort is asked for");

    // Not while the assistant is answering.
    harness.click_label("Effort: Default");
    harness.idle(3);
    assert!(!harness.shows("xhigh"), "the list does not open");
    assert!(harness.app.assistant_mut().choose_effort(Some(Effort::Max)).is_err());
    script.say([started(), text("Hello."), finished()]);
    harness.idle(3);

    // The list: the CLI's own, and the levels its `--effort` takes.
    harness.click_label("Effort: Default");
    harness.idle(3);
    assert!(harness.is_on("Default"));
    for effort in Effort::ALL {
        assert!(harness.shows(effort.name()) && !harness.is_on(effort.name()), "{}", effort.name());
    }
    harness.click_label("high");
    harness.idle(3);
    assert_eq!(harness.app.assistant().effort_choice(), Some(Effort::High));
    assert!(harness.shows("Effort: high") && !harness.shows("xhigh"), "chosen, and the list is put away");

    // It takes hold with the next message, in the same conversation.
    harness.tell("again");
    let state = script.state();
    assert_eq!(state.opened, [None, Some(conversation_id('a'))]);
    assert_eq!(state.setups[1].effort, Some(Effort::High));
    assert_eq!(state.setups[1].model, None, "the model is as it was");
    drop(state);
    script.say([started(), finished()]);
    harness.idle(3);
    // And it is said where the model that answers is said.
    let header = harness.passage("New conversation");
    harness.move_to(pos2(SCREEN.x - harness.app.assistant().width() + 40.0, header.center().y));
    harness.idle(2);
    assert!(harness.hint().contains("answering as test-model at high effort"), "{}", harness.hint());

    // Back to the CLI's own.
    harness.click_label("Effort: high");
    harness.click_label("Default");
    harness.idle(3);
    harness.tell("once more");
    assert_eq!(script.state().setups[2].effort, None);
}

#[test]
fn the_step_limit_is_in_sight_and_can_be_moved() {
    let (mut harness, script, tracks, _) = assisted();
    assert!(harness.shows("Limit: 1,000 steps"), "the bound on one request is said, not hidden");
    assert_eq!(harness.app.assistant().step_limit(), trill_assistant::DEFAULT_MAX_TURNS);

    harness.click_label("Limit: 1,000 steps");
    harness.idle(3);
    for offered in ["250 steps", "500 steps", "1,000 steps", "2,000 steps"] {
        assert!(harness.shows(offered), "{offered}");
    }
    assert!(harness.is_on("1,000 steps"));
    harness.click_label("250 steps");
    harness.idle(3);
    assert_eq!(harness.app.assistant().step_limit(), 250);
    assert!(harness.shows("Limit: 250 steps"));

    // It is what the back end is given, and what the turn counts against.
    harness.tell("write the whole arrangement");
    assert_eq!(script.state().setups[0].step_limit, 250);
    script.say([started()]);
    harness.work_through(&script, tracks[0], 1, 30);
    assert!(harness.shows_start("Working\u{2026} \u{00B7} Step 30 of 250"), "{:?}", harness.said_in_the_panel());
    // Not to be changed under a turn's feet.
    assert!(harness.app.assistant_mut().set_step_limit(2000).is_err());

    // At the limit the turn ends, with what it did one step, and offers to
    // carry on: with the same limit again.
    script.say([progress(250, Some(2.5), 250), Event::Failed(FailureReason::TurnLimitReached { limit: 250 })]);
    harness.idle(3);
    assert!(harness.shows_start("The assistant stopped at the limit of 250 steps for one request."));
    assert!(harness.shows("Undo"));
    harness.click_label("Continue");
    assert_eq!(script.state().sent[1].message, "Continue.");
    assert_eq!(script.state().opened.len(), 1, "in the same conversation, opened as it was");
}

#[test]
fn a_conversation_that_outgrows_the_model_is_summarised_in_plain_sight_and_the_turn_goes_on() {
    let (mut harness, script, tracks, _) = assisted();
    harness.tell("write a whole song");
    script.say([started()]);
    harness.work_through(&script, tracks[0], 1, 20);
    script.say([Event::Notice(Notice::Compacting)]);
    harness.idle(2);
    assert!(harness.shows_start("The conversation has grown past what the model can hold at once. Claude Code is"));

    script.say([Event::Notice(Notice::Compacted { tokens_before: Some(985_053), tokens_after: Some(644) })]);
    harness.idle(2);
    let notes: Vec<&str> = harness
        .items()
        .iter()
        .filter_map(|item| if let Item::Note { text, .. } = item { Some(text.as_str()) } else { None })
        .collect();
    assert_eq!(
        notes,
        ["The conversation had grown past what the model can hold at once (985,053 tokens), so Claude Code \
          summarised the earlier part (644 tokens now) and is carrying on. The song and the plan are untouched; \
          what the assistant remembers of its early steps is now the summary."],
        "one line, which says what happened"
    );
    assert!(harness.shows(notes[0]));
    assert!(harness.app.assistant().is_working(), "and the turn goes on by itself");

    harness.work_through(&script, tracks[0], 21, 5);
    script.say([text("Carried on."), finished()]);
    harness.idle(3);
    assert!(matches!(harness.ending(), (Ending::Finished { .. }, true)));
    assert_eq!(script.state().sent.len(), 1);
}

#[test]
fn what_is_chosen_under_the_message_field_is_remembered_from_one_session_to_the_next() {
    use trill_assistant::Effort;
    let home = tempfile::tempdir().unwrap();
    let start = || {
        let storage = trill_runtime::Storage::At { settings: home.path().join("settings.json"), library_index: None };
        Arc::new(Runtime::start(RuntimeOptions { storage, ..RuntimeOptions::hermetic() }).unwrap())
    };
    let kept = || std::fs::read_to_string(home.path().join("assistant.json")).unwrap();

    let mut harness = Harness::on(start(), UiOptions::default(), |_| {});
    harness.open_assistant(&Script::ready());
    harness.click_label("Effort: Default");
    harness.click_label("xhigh");
    harness.idle(3);
    harness.click_label("Web research");
    harness.idle(2);
    harness.click_label("Limit: 1,000 steps");
    harness.click_label("2,000 steps");
    harness.idle(3);
    drop(harness);
    let file = kept();
    assert!(file.contains("\"effort\": \"xhigh\"") && file.contains("\"web\": false"), "{file}");
    assert!(file.contains("\"step_limit\": 2000"), "{file}");

    // The next session starts with all three, shows them, and opens its
    // conversation with them.
    let mut harness = Harness::on(start(), UiOptions::default(), |_| {});
    let script = Script::ready();
    harness.app.assistant_mut().use_script(script.clone());
    harness.idle(3);
    let assistant = harness.app.assistant();
    assert_eq!(
        (assistant.effort_choice(), assistant.web(), assistant.step_limit()),
        (Some(Effort::ExtraHigh), false, 2000)
    );
    assert!(harness.shows("Effort: xhigh") && harness.shows("Limit: 2,000 steps") && !harness.is_on("Web research"));
    harness.tell("hello");
    assert_eq!(
        script.state().setups,
        [Setup { model: None, effort: Some(Effort::ExtraHigh), web: false, step_limit: 2000 }]
    );
    script.say([started(), finished()]);
    harness.idle(3);

    // Put back as they were, they go from the file again.
    harness.click_label("Effort: xhigh");
    harness.click_label("Default");
    harness.idle(3);
    harness.click_label("Web research");
    harness.idle(2);
    harness.click_label("Limit: 2,000 steps");
    harness.click_label("1,000 steps");
    harness.idle(3);
    drop(harness);
    let file = kept();
    assert!(!file.contains("effort") && !file.contains("step_limit") && file.contains("\"web\": true"), "{file}");
}

#[test]
fn a_conversation_begun_under_earlier_instructions_is_not_resumed_and_the_panel_says_so() {
    let home = tempfile::tempdir().unwrap();
    let start = || {
        let storage = trill_runtime::Storage::At { settings: home.path().join("settings.json"), library_index: None };
        Arc::new(Runtime::start(RuntimeOptions { storage, ..RuntimeOptions::hermetic() }).unwrap())
    };
    let song = home.path().join("songs").join("night.trill");
    let mut harness = Harness::on(start(), UiOptions::default(), |session| {
        three_tracks(session);
        session.save(&song).unwrap();
    });
    let script = Script::ready();
    harness.open_assistant(&script);
    harness.tell("remember the word marimba");
    script.say([started(), text("I will."), finished()]);
    harness.idle(3);
    drop(harness);

    // As a Trill that told the assistant something else left it. The CLI
    // keeps a conversation to the instructions it began with: that one has
    // never been told what the web is, or that a page is not to be obeyed.
    let file = home.path().join("assistant.json");
    let kept = std::fs::read_to_string(&file).unwrap();
    let (before, after) = kept.split_once("\"instructions\": \"").expect("the mark is kept with the conversation");
    std::fs::write(&file, format!("{before}\"instructions\": \"0000000000000000{}", &after[16..])).unwrap();

    let mut harness = Harness::on(start(), UiOptions::default(), |session| session.load(&song).unwrap());
    let script = Script::ready();
    harness.app.assistant_mut().use_script(script.clone());
    harness.idle(5);
    assert!(
        harness.shows_start("Trill has given the assistant new instructions since the last conversation"),
        "{:?}",
        harness.said_in_the_panel()
    );
    assert!(!harness.is_on_screen("I will."), "the conversation it cannot carry on is not shown as if it could");
    harness.tell("what was the word?");
    assert_eq!(script.state().opened, [None], "a new conversation, not the old one resumed");
    let begun = TurnStart { session_id: conversation_id('b'), model: "test-model".into(), api_key_source: None };
    script.say([Event::Started(begun), text("I do not know."), finished()]);
    harness.idle(3);
    drop(harness);

    // From here on it is that one that is taken up again.
    let kept = std::fs::read_to_string(&file).unwrap();
    assert!(kept.contains(conversation_id('b').as_str()) && !kept.contains(conversation_id('a').as_str()), "{kept}");
    let mut harness = Harness::on(start(), UiOptions::default(), |session| session.load(&song).unwrap());
    let script = Script::ready();
    harness.app.assistant_mut().use_script(script.clone());
    wait_for_the_conversation(&mut harness);
    harness.tell("and now?");
    assert_eq!(script.state().opened, [Some(conversation_id('b'))]);
}

// ---- against the CLI installed on this machine ------------------------------------------

/// The panel driven against the real thing: the CLI installed and signed in
/// on this machine, and a real MCP server. These spend the usage of whoever
/// runs them (seven short messages, six of them to the smallest model), so
/// each does nothing unless asked:
///
/// ```text
/// TRILL_TEST_CLAUDE=1 cargo test -p trill-app live_ -- --test-threads=1 --nocapture
/// ```
///
/// Settings, the CLI's working folder and the kept conversations go into a
/// temporary folder; nothing of the user's own settings is read or written.
/// (The CLI keeps its own record of each conversation in its own data
/// folder, as it does for any conversation.)
mod live {
    use super::*;

    /// The longest a real turn may take before the test gives up.
    const TURN_TIMEOUT: Duration = Duration::from_secs(180);

    fn asked_for() -> bool {
        let asked = std::env::var("TRILL_TEST_CLAUDE").as_deref() == Ok("1");
        if !asked {
            eprintln!("skipped: set TRILL_TEST_CLAUDE=1 to run against the real CLI");
        }
        asked
    }

    /// Runs frames, a few a second, until `done` or the timeout.
    fn wait(harness: &mut Harness, what: &str, done: impl Fn(&Harness) -> bool) {
        let deadline = Instant::now() + TURN_TIMEOUT;
        while !done(harness) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}; so far: {:#?}", harness.items());
            std::thread::sleep(Duration::from_millis(30));
            harness.idle(1);
        }
    }

    /// Opens the window on settings kept in `home`, with the real CLI
    /// behind the assistant, and waits until it has been found.
    fn start(home: &std::path::Path, setup: impl FnOnce(&mut Session)) -> Harness {
        let storage = trill_runtime::Storage::At { settings: home.join("settings.json"), library_index: None };
        let runtime = Arc::new(Runtime::start(RuntimeOptions { storage, ..RuntimeOptions::hermetic() }).unwrap());
        let mut harness = Harness::on(runtime.clone(), UiOptions::default(), setup);
        harness.app.assistant_mut().use_installed_cli(runtime, home.to_owned(), "haiku");
        if !harness.app.assistant().is_open() {
            harness.key(CTRL_SHIFT, Key::A);
        }
        // A frame or two for the panel to start looking; what was on screen
        // until now was the answer of the back end that was replaced.
        harness.idle(2);
        assert!(harness.is_on_screen("Looking for Claude Code\u{2026}."), "the CLI is looked for off this thread");
        wait(&mut harness, "the CLI to be found", |harness| {
            harness.is_on_screen("Send") || harness.is_on_screen("Check again")
        });
        assert!(harness.is_on_screen("Send"), "the assistant is not available: {:?}", harness.labels);
        harness
    }

    /// Sends `message` and waits for the turn to end.
    fn turn(harness: &mut Harness, message: &str) {
        eprintln!("\n> {message}");
        let from = harness.items().len();
        harness.tell(message);
        assert!(harness.app.assistant().is_working(), "the message was not sent");
        wait(harness, "the turn to end", |harness| !harness.app.assistant().is_working());
        harness.idle(2);
        for item in &harness.items()[from..] {
            match item {
                Item::User { .. } => {}
                Item::Reply { text } => eprintln!("  said: {text}"),
                Item::Tool(line) => eprintln!("  tool: {} [{}] {:?}", line.words(), line.tool, line.state),
                Item::Note { text, .. } => eprintln!("  note: {text}"),
                Item::End(end) => eprintln!("  end: {:?}, changed the song: {}", end.ending, end.changed),
            }
        }
    }

    fn tools_called(harness: &Harness) -> Vec<String> {
        let lines = harness.items().iter().filter_map(|item| match item {
            Item::Tool(line) => Some(line.tool.clone()),
            _ => None,
        });
        lines.collect()
    }

    #[test]
    fn live_a_message_goes_with_the_selection_and_its_edit_is_one_undo() {
        if !asked_for() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let mut zebra = None;
        let mut harness = start(home.path(), |session| {
            let (_, clips) = three_tracks(session);
            let rename = |project: &mut Project| {
                project.clip_mut(clips[1])?.name = "Zebra".to_owned();
                Ok(())
            };
            session.edit(rename).unwrap();
            zebra = Some(clips[1]);
        });
        let zebra = zebra.unwrap();
        harness.click(harness.lane(17.0, 0));
        assert_eq!(harness.app.state().selected_clip, Some(zebra));
        harness.idle(2);
        let depth = harness.undo_depth();

        // 1: only the context says which clip is selected.
        turn(
            &mut harness,
            "Without calling any tool: what is the name of the clip I have selected? Reply with the name only.",
        );
        assert!(matches!(harness.ending().0, Ending::Finished { .. }), "{:?}", harness.ending());
        assert!(
            harness.reply().to_lowercase().contains("zebra"),
            "the reply does not name the selected clip: {}",
            harness.reply()
        );
        assert_eq!(harness.undo_depth(), depth, "a question changes nothing");
        assert!(
            !harness.labels.iter().any(|(label, _)| label.contains("billed")),
            "a Claude login is not billed per message"
        );

        // 2: "that clip" is the selected one; the edit is one step, and the
        // turn's own Undo takes it back.
        turn(&mut harness, "Rename that clip to Giraffe.");
        assert_eq!(harness.project().clip(zebra).unwrap().name, "Giraffe");
        assert!(tools_called(&harness).iter().any(|tool| tool == "update_clip"), "{:?}", tools_called(&harness));
        assert!(harness.ending().1, "the turn is marked as having changed the song");
        assert_eq!(harness.undo_depth(), depth + 1, "everything the turn changed is one step");
        assert!(harness.shows("Undo"));
        harness.click_label("Undo");
        assert_eq!(harness.project().clip(zebra).unwrap().name, "Zebra");
        assert!(harness.app.assistant().conversation().is_some());
    }

    #[test]
    fn live_the_chosen_model_answers_and_the_window_goes_to_what_it_made() {
        if !asked_for() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let mut tracks = None;
        let mut harness = start(home.path(), |session| tracks = Some(three_tracks(session).0));
        let tracks = tracks.unwrap();
        harness.app.state_mut().detail_tab = DetailTab::Mixer;

        // 1: a model's full name, typed into the chooser (the smallest
        // model, by the name the CLI itself lists for it). The tool's real
        // answer is what leads the window to the clip.
        harness.click_label("Model: haiku");
        harness.click_label("Another model name");
        harness.type_text("claude-haiku-5-5");
        harness.key(Modifiers::NONE, Key::Enter);
        harness.idle(3);
        assert_eq!(harness.app.assistant().model_choice(), Some("claude-haiku-5-5"));
        turn(
            &mut harness,
            "On the track named Three, create an empty MIDI clip named Marimba that starts at beat 64 and is 8 beats \
             long. Make exactly that one change and nothing else, then reply with OK only.",
        );
        assert!(matches!(harness.ending().0, Ending::Finished { .. }), "{:?}", harness.ending());
        let first = harness.app.assistant().reported_model().expect("the CLI names the model").to_owned();
        eprintln!("  asked for claude-haiku-5-5; the CLI says it answers as {first}");
        assert!(first.contains("haiku"), "{first}");
        let project = harness.project();
        let made = project.track(tracks[2]).unwrap().clips.iter().find(|clip| clip.name == "Marimba");
        let made = made.unwrap_or_else(|| panic!("no clip named Marimba on Three: {:?}", tools_called(&harness))).id;
        harness.idle(30);
        let led = harness.items().iter().any(|item| matches!(item, Item::Tool(line) if line.place.is_some()));
        assert!(led, "a call's real answer said what it made: {:#?}", harness.items());
        assert_eq!(harness.selected_clips(), [made], "the window went to the clip when the turn ended");
        assert_eq!(harness.app.state().selected_track, Some(tracks[2]));
        assert_eq!(harness.app.state().detail_tab, DetailTab::Clip);
        let (from, to) = harness.beats_in_view();
        assert!(from <= 64.0 && to >= 72.0, "the timeline shows the clip: {from} to {to}");
        let conversation = harness.app.assistant().conversation().cloned().expect("the conversation has an id");

        // 2: another model, by the CLI's name for its family, carries the
        // same conversation on: it knows what was done before it came.
        harness.click_label("Model: claude-haiku-5-5");
        harness.click_label("sonnet");
        harness.idle(3);
        assert!(harness.shows(
            "The model \u{201C}sonnet\u{201D} answers from the next message on. The conversation carries on with it."
        ));
        turn(
            &mut harness,
            "Without calling any tool: what is the name of the clip you created a moment ago? Reply with the name only.",
        );
        assert!(matches!(harness.ending().0, Ending::Finished { .. }), "{:?}", harness.ending());
        let second = harness.app.assistant().reported_model().expect("the CLI names the model").to_owned();
        eprintln!("  asked for sonnet; the CLI says it answers as {second}");
        assert!(second.contains("sonnet") && second != first, "{second}");
        assert!(
            harness.reply().to_lowercase().contains("marimba"),
            "it was not handed the conversation: {}",
            harness.reply()
        );
        assert_eq!(harness.app.assistant().conversation(), Some(&conversation), "it is the same conversation");
    }

    #[test]
    fn live_stop_ends_a_turn_and_closes_its_group() {
        if !asked_for() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let mut harness = start(home.path(), |session| {
            three_tracks(session);
        });
        let depth = harness.undo_depth();
        let message = "Create six new tracks named Stop 1 to Stop 6. Make one tool call per track, one after the \
                       other, never two at once.";
        eprintln!("\n> {message}");
        harness.tell(message);
        assert!(harness.app.assistant().is_working());
        // Let it get as far as a tool call that has changed the song, then
        // stop it.
        wait(&mut harness, "the first track", |harness| {
            let made = |item: &Item| matches!(item, Item::Tool(line) if line.state == ToolState::Done);
            !harness.app.assistant().is_working() || harness.items().iter().any(made)
        });
        let stopped = harness.app.assistant().is_working();
        if stopped {
            harness.click_last_label("Stop");
            assert!(harness.app.assistant().is_stopping());
        }
        wait(&mut harness, "the turn to stop", |harness| !harness.app.assistant().is_working());
        harness.idle(2);
        let tracks = harness.project().tracks.len() - 3;
        eprintln!("  stopped: {stopped}; tracks made before the stop: {tracks}; end: {:?}", harness.ending());
        for item in harness.items() {
            if let Item::Tool(line) = item {
                eprintln!("  tool: {} [{}] {:?}", line.words(), line.tool, line.state);
            }
        }
        if stopped {
            assert_eq!(harness.ending().0, Ending::Stopped);
        }
        assert!(tracks > 0, "it got as far as making a track");
        assert_eq!(harness.ending().1, tracks > 0);
        assert_eq!(harness.undo_depth(), depth + usize::from(tracks > 0), "what was done before the stop is one step");
        // The group is closed: the next edit is its own step, and the
        // field is there for the next message.
        harness.session.lock().edit(|project| project.set_tempo(100.0)).unwrap();
        assert_eq!(harness.undo_depth(), depth + usize::from(tracks > 0) + 1);
        harness.idle(2);
        assert!(harness.is_on_screen("Send"));
    }

    #[test]
    fn live_a_conversation_is_taken_up_again_after_a_restart() {
        if !asked_for() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let song = home.path().join("song.trill");
        let mut harness = start(home.path(), |session| {
            three_tracks(session);
            session.save(&song).unwrap();
        });
        turn(&mut harness, "Remember this word for later: marimba. Reply with OK only.");
        assert!(matches!(harness.ending().0, Ending::Finished { .. }), "{:?}", harness.ending());
        let conversation = harness.app.assistant().conversation().cloned().expect("the conversation has an id");
        drop(harness);

        // Another run of the program, the same project.
        let mut harness = start(home.path(), |session| session.load(&song).unwrap());
        wait(&mut harness, "the kept conversation", |harness| !harness.items().is_empty());
        assert!(harness.is_on_screen("You: Remember this word for later: marimba. Reply with OK only."));
        turn(&mut harness, "What was the word I asked you to remember? Reply with the word only.");
        assert!(harness.reply().to_lowercase().contains("marimba"), "it does not remember: {}", harness.reply());
        assert_eq!(harness.app.assistant().conversation(), Some(&conversation), "it is the same conversation");
    }

    #[test]
    fn live_the_tools_that_write_files_are_offered_and_kept_to_the_working_folders() {
        if !asked_for() {
            return;
        }
        // The project lives in one temporary folder; the assistant is asked
        // to write into another, which is neither the project's nor Trill's.
        let home = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let song = home.path().join("song.trill");
        let outside = elsewhere.path().join("outside.trill");
        let mut harness = start(home.path(), |session| {
            three_tracks(session);
            session.save(&song).unwrap();
        });
        turn(
            &mut harness,
            &format!(
                "Save a copy of the project as {}. Try once. If the tool refuses, do not try anything else: tell me \
                 what it said.",
                outside.display()
            ),
        );
        let saves: Vec<_> = harness
            .items()
            .iter()
            .filter_map(|item| match item {
                Item::Tool(line) if line.tool == "save_project" => Some(line.clone()),
                _ => None,
            })
            .collect();
        assert!(!saves.is_empty(), "save_project is offered, and was called: {:?}", tools_called(&harness));
        for save in &saves {
            eprintln!("  the server said: {}", save.output);
        }
        assert_eq!(saves[0].state, ToolState::Failed, "the server refuses a path outside the working folders");
        assert!(!outside.exists(), "and nothing was written there");
        assert_eq!(harness.session.lock().path(), Some(song.as_path()), "the project is still where it was");
    }
}
