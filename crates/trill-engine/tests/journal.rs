//! The journal a session keeps, through the session's own doors: edits,
//! drags, calls, undo and redo, groups, saving and opening.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use trill_core::journal::{Author, Entry, Journal, Outcome, PlanItem, PlanState, Reversal, States, What};
use trill_core::{ChainRef, ClipId, DeviceId, Note, Project, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::journal::{acting, file_for};
use trill_engine::{AudioBackend, Session};

fn session() -> Session {
    Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: 48_000 }).unwrap()
}

fn journal(session: &Session) -> &Journal {
    session.journal().expect("a journal is kept unless it is switched off")
}

fn lines(session: &Session) -> Vec<String> {
    journal(session).entries().filter(|entry| !entry.is_aside()).map(Entry::line).collect()
}

fn last(session: &Session) -> &Entry {
    journal(session).last().expect("the journal has an entry")
}

/// The journal leads to the song as it is, byte for byte.
fn assert_in_step(session: &Session) {
    assert_eq!(&journal(session).head().unwrap(), session.project(), "the journal ends where the song is");
}

fn note_at(start: f64, pitch: u8) -> Note {
    Note { pitch, start, length: 0.5, velocity: 100 }
}

/// A track with a synth and a clip of eight notes, made in three edits.
fn bass(session: &mut Session) -> (TrackId, DeviceId, ClipId) {
    let track = session.edit(|p| Ok(p.add_track("Bass"))).unwrap();
    let synth = session.add_device(ChainRef::Track(track), "builtin:polysynth", None, None).unwrap();
    let clip = session
        .edit(|p| {
            let clip = p.add_midi_clip(track, "Line", 0.0, 8.0)?;
            let notes: Vec<Note> = (0..8).map(|step| note_at(step as f64, 36 + step)).collect();
            p.clip_mut(clip)?.add_notes(&notes)?;
            Ok(clip)
        })
        .unwrap();
    (track, synth, clip)
}

#[test]
fn every_edit_is_a_step_that_says_who_made_it_what_it_was_and_where() {
    let mut session = session();
    assert!(journal(&session).is_empty(), "a new project's story has not begun");
    let began = journal(&session).base(States::Without).unwrap();
    assert_eq!(began.master.devices.len(), 1, "it begins with the song as it was: a limiter on the master");

    let (track, synth, clip) = bass(&mut session);
    session.edit(|p| p.set_tempo(96.0)).unwrap();
    session.set_device_params(synth, &[("cutoff".to_string(), 900.0)]).unwrap();
    assert_eq!(
        lines(&session),
        [
            "Added the track \u{201C}Bass\u{201D}",
            "Added Poly Synth to \u{201C}Bass\u{201D}",
            "Added the clip \u{201C}Line\u{201D} to \u{201C}Bass\u{201D}",
            "Set the tempo to 96 BPM",
            "Adjusted \u{201C}Poly Synth\u{201D}",
        ]
    );
    assert!(journal(&session).entries().all(|entry| entry.by == Author::Person));
    assert_in_step(&session);

    // The song as it was after each of them.
    let journal = journal(&session);
    let with = States::With(journal.blobs());
    assert_eq!(journal.song_at(1, with).unwrap().track(track).unwrap().devices.len(), 0);
    assert_eq!(journal.song_at(2, with).unwrap().device(synth).unwrap().params.get("cutoff"), None);
    assert!(journal.song_at(2, with).unwrap().clip(clip).is_err());
    assert_eq!(journal.song_at(3, with).unwrap().clip(clip).unwrap().notes().len(), 8);
    assert_eq!(journal.song_at(3, with).unwrap().tempo, 120.0);

    // A refused edit and one that changes nothing leave no entry.
    let entries = journal.len();
    assert!(session.edit(|p| p.set_tempo(-4.0)).is_err());
    session.edit(|p| p.set_tempo(96.0)).unwrap();
    assert!(session.add_device(ChainRef::Track(track), "builtin:nope", None, None).is_err());
    assert_eq!(self::journal(&session).len(), entries);
}

#[test]
fn a_drag_is_one_step_and_a_step_taken_back_stays_in_the_story() {
    let mut session = session();
    let (track, ..) = bass(&mut session);

    session.begin_gesture();
    for db in 1..=12 {
        session.edit(|p| p.set_track_volume(track, -(db as f32))).unwrap();
    }
    session.end_gesture();
    assert_eq!(lines(&session).len(), 4);
    assert_eq!(last(&session).line(), "Changed the volume of \u{201C}Bass\u{201D}");
    assert_in_step(&session);

    // Undo: the step stays, and one that takes it back follows.
    assert!(session.undo());
    assert_eq!(lines(&session).len(), 5);
    assert_eq!(last(&session).line(), "Took back: Changed the volume of \u{201C}Bass\u{201D}");
    let reversed = last(&session).step().unwrap().reverses.expect("it says which step it took back");
    assert_eq!((reversed.kind, reversed.first, reversed.last), (Reversal::Undo, 4, 4));
    assert_eq!(session.project().track(track).unwrap().volume_db, 0.0);
    assert_in_step(&session);
    // The song as it was with the fader down is still there to be seen.
    let journal = journal(&session);
    assert_eq!(journal.song_at(4, States::Without).unwrap().track(track).unwrap().volume_db, -12.0);

    // Redo brings it back, as another step.
    assert!(session.redo());
    assert_eq!(last(&session).line(), "Brought back: Changed the volume of \u{201C}Bass\u{201D}");
    assert_eq!(last(&session).step().unwrap().reverses.map(|reversed| reversed.kind), Some(Reversal::Redo));
    assert_in_step(&session);

    // Several undone in a row, and an edit made from there.
    assert!(session.undo() && session.undo());
    assert_eq!(last(&session).line(), "Took back: Added the clip \u{201C}Line\u{201D} to \u{201C}Bass\u{201D}");
    session.edit(|p| p.set_track_pan(track, 0.5)).unwrap();
    assert_eq!(last(&session).line(), "Changed the pan of \u{201C}Bass\u{201D}");
    assert_in_step(&session);
}

#[test]
fn what_a_call_does_is_its_own_step_under_its_words_whoever_else_is_at_work() {
    let mut session = session();
    let (track, synth, clip) = bass(&mut session);
    let client = || Author::Client("claude-code".into());

    // A call that makes several edits: one step.
    let call = session.begin_call(client(), "write_bass", "Wrote a bass line in \u{201C}Line\u{201D}").unwrap();
    {
        let _acting = acting(call);
        session.edit(|p| p.clip_mut(clip)?.set_notes(vec![note_at(0.0, 40), note_at(2.0, 43)])).unwrap();
        session.edit(|p| p.set_clip_span(clip, 0.0, 4.0)).unwrap();
    }
    // Meanwhile the person turns a knob: their own step, in between.
    session.set_device_params(synth, &[("cutoff".to_string(), 500.0)]).unwrap();
    {
        let _acting = acting(call);
        session.edit(|p| p.clip_mut(clip)?.set_loop_length(Some(4.0))).unwrap();
    }
    session.end_call(call, Outcome::default());
    let ends: Vec<(Author, String)> =
        journal(&session).entries().rev().take(3).map(|entry| (entry.by.clone(), entry.line())).collect();
    assert_eq!(
        ends,
        [
            (client(), "Wrote a bass line in \u{201C}Line\u{201D}".to_owned()),
            (Author::Person, "Adjusted \u{201C}Poly Synth\u{201D}".to_owned()),
            (client(), "Wrote a bass line in \u{201C}Line\u{201D}".to_owned()),
        ]
    );
    assert_eq!(last(&session).step().unwrap().tool.as_deref(), Some("write_bass"));
    assert_in_step(&session);

    // A call that only looks is a look, with what it heard.
    let entries = journal(&session).len();
    let call = session.begin_call(Author::Assistant, "measure", "Measured the song").unwrap();
    session.end_call(call, Outcome { heard: Some("-18.3 LUFS".into()), ..Outcome::default() });
    assert!(
        matches!(&last(&session).what, What::Look(look) if look.heard.as_deref() == Some("-18.3 LUFS") && !look.failed)
    );
    assert_eq!(journal(&session).len(), entries + 1);

    // A call that fails halfway and backs out with an undo of its own
    // leaves the story as if it had not tried: only the call, failed.
    let before = session.project().clone();
    let call = session.begin_call(Author::Assistant, "create_track", "Added the track \u{201C}Pad\u{201D}").unwrap();
    {
        let _acting = acting(call);
        let pad = session.edit(|p| Ok(p.add_track("Pad"))).unwrap();
        assert!(session.add_device(ChainRef::Track(pad), "builtin:polysynth", Some("no such preset"), None).is_err());
        assert!(session.undo(), "the tool backs out");
    }
    session.end_call(call, Outcome { failed: true, ..Outcome::default() });
    assert_eq!(session.project(), &before);
    assert_eq!(journal(&session).len(), entries + 2);
    assert!(matches!(&last(&session).what, What::Look(look) if look.failed && look.label.contains("Pad")));
    assert_in_step(&session);

    // The assistant undoes a step of the person's through a tool: that is
    // a step taken back, by the assistant.
    let call = session.begin_call(Author::Assistant, "history", "Undid a step").unwrap();
    {
        let _acting = acting(call);
        assert!(session.undo());
    }
    session.end_call(call, Outcome::default());
    assert_eq!(last(&session).by, Author::Assistant);
    assert!(last(&session).line().starts_with("Took back: "), "{}", last(&session).line());
    assert_in_step(&session);
    let _ = track;
}

#[test]
fn a_request_of_the_assistants_is_one_undo_and_is_taken_back_as_one_step_of_the_story() {
    let mut session = session();
    let (track, ..) = bass(&mut session);
    let before = session.project().clone();

    session.say(Author::Person, "Add drums and turn the bass down.");
    session.note_plan(vec![
        PlanItem { text: "Add a drum track".into(), state: PlanState::Doing },
        PlanItem { text: "Turn the bass down".into(), state: PlanState::Pending },
    ]);
    let group = session.begin_group();
    for (tool, label) in [
        ("create_track", "Added the track \u{201C}Drums\u{201D}"),
        ("update_track", "Changed the volume of \u{201C}Bass\u{201D}"),
    ] {
        let call = session.begin_call(Author::Assistant, tool, label).unwrap();
        {
            let _acting = acting(call);
            match tool {
                "create_track" => drop(session.edit(|p| Ok(p.add_track("Drums"))).unwrap()),
                _ => session.edit(|p| p.set_track_volume(track, -6.0)).unwrap(),
            }
        }
        session.end_call(call, Outcome::default());
    }
    session.say(Author::Assistant, "Done.");
    assert!(session.end_group(group).is_some());
    let entries = journal(&session).len();
    assert_eq!(
        lines(&session)[3..],
        [
            "Add drums and turn the bass down.",
            "Plan: 0 of 2 done",
            "Added the track \u{201C}Drums\u{201D}",
            "Changed the volume of \u{201C}Bass\u{201D}",
            "Done."
        ]
    );

    // One undo takes the request back, and the story says so in one step.
    assert!(session.undo());
    assert_eq!(session.project(), &before);
    assert_eq!(journal(&session).len(), entries + 1);
    assert_eq!(last(&session).line(), "Took back 2 steps, from: Added the track \u{201C}Drums\u{201D}");
    assert_eq!(last(&session).by, Author::Person);
    assert!(session.redo());
    assert_eq!(last(&session).line(), "Brought back 2 steps, from: Added the track \u{201C}Drums\u{201D}");
    assert_in_step(&session);
}

/// A folder for a session to keep journals in, and one for its projects.
fn folders() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let (journals, songs) = (root.path().join("program").join("journals"), root.path().join("songs"));
    std::fs::create_dir_all(&songs).unwrap();
    (root, journals, songs)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn saving_writes_the_journal_in_the_programs_own_folder_and_nothing_of_it_into_the_song() {
    let (_root, journals, songs) = folders();
    let mut session = session();
    session.keep_journals_in(journals.clone());
    let (track, ..) = bass(&mut session);
    session.say(Author::Person, "A secret between me and the assistant.");
    session.say(Author::Assistant, "Kept.");

    let song = songs.join("Night Drive.trill");
    session.save(&song).unwrap();

    // The song's folder holds the song and nothing else; the song holds
    // nothing that was said, and no trace of a journal.
    let beside: Vec<String> = std::fs::read_dir(&songs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(beside, ["Night Drive.trill"]);
    let project = read(&song);
    assert!(!project.contains("secret") && !project.contains("journal") && !project.contains("Kept."));

    // The journal is in the program's folder, under a name that says
    // nothing of the song, and it does hold what was said.
    let file = file_for(&journals, &song);
    let kept = read(&file);
    assert!(kept.contains("A secret between me and the assistant."));
    assert!(!file.file_name().unwrap().to_string_lossy().to_lowercase().contains("night"));

    // Saving again adds what happened since and rewrites nothing.
    session.edit(|p| p.set_track_volume(track, -3.0)).unwrap();
    session.save(&song).unwrap();
    let longer = read(&file);
    assert!(longer.starts_with(&kept), "the file is added to");
    assert_eq!(longer.lines().count(), kept.lines().count() + 1);
    // Nothing happened: nothing is written.
    let written = std::fs::metadata(&file).unwrap().modified().unwrap();
    session.save(&song).unwrap();
    assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), written);

    // Saved under another name, the journal goes along; the first file
    // keeps the story up to where it was left.
    let copy = songs.join("Night Drive 2.trill");
    session.edit(|p| p.set_track_pan(track, 0.25)).unwrap();
    session.save(&copy).unwrap();
    assert!(read(&file_for(&journals, &copy)).contains("Changed the pan"));
    assert!(!read(&file).contains("Changed the pan"));

    // Handed on by name: without the conversation unless asked for.
    let handed = songs.join("making-of.trillstory");
    session.export_journal(&handed, false).unwrap();
    let text = read(&handed);
    assert!(text.contains("Added the track") && !text.contains("secret") && !text.contains("Kept."));
    assert_eq!(Journal::from_text(&text).unwrap().head().unwrap(), *session.project());
    session.export_journal(&handed, true).unwrap();
    assert!(read(&handed).contains("A secret between me and the assistant."));
}

#[test]
fn a_song_that_is_opened_takes_its_story_up_where_it_was_left() {
    let (_root, journals, songs) = folders();
    let song = songs.join("song.trill");
    let (told, saved_project) = {
        let mut session = session();
        session.keep_journals_in(journals.clone());
        let (track, ..) = bass(&mut session);
        session.say(Author::Person, "Make it warmer.");
        session.save(&song).unwrap();
        // Changed after the save and never saved: not part of the file's
        // story.
        session.edit(|p| p.set_track_volume(track, -9.0)).unwrap();
        (lines(&session), read(&song))
    };

    // Another run of the program.
    let mut session = session();
    session.keep_journals_in(journals.clone());
    session.load(&song).unwrap();
    assert_eq!(lines(&session), told[..told.len() - 1], "the story as it was when the song was saved");
    assert_in_step(&session);
    assert!(!session.can_undo(), "the undo history does not carry over; the story does");
    // And it goes on from there.
    let track = session.project().tracks[0].id;
    session.edit(|p| p.set_track_pan(track, -0.5)).unwrap();
    assert_eq!(last(&session).seq as usize, told.len());
    session.save(&song).unwrap();

    // The file is changed where no journal is kept (another machine, or
    // with the journal switched off): the next time it is opened here, a
    // step says so and the story is whole again.
    let mut elsewhere = self::session();
    elsewhere.load(&song).unwrap();
    elsewhere.edit(|p| Ok(p.add_track("Added elsewhere"))).unwrap();
    elsewhere.save(&song).unwrap();
    let mut session = self::session();
    session.keep_journals_in(journals.clone());
    session.load(&song).unwrap();
    assert_eq!(last(&session).line(), "The song was changed while no journal was kept");
    assert_eq!(lines(&session).len(), told.len() + 1);
    assert_in_step(&session);

    // A song without a journal begins one with the song as it is.
    std::fs::write(songs.join("other.trill"), saved_project).unwrap();
    session.load(&songs.join("other.trill")).unwrap();
    assert!(journal(&session).is_empty());
    assert_eq!(journal(&session).base(States::Without).unwrap().tracks.len(), 1);
    // A new project begins another, empty.
    session.new_project();
    assert!(journal(&session).is_empty());
    assert_eq!(journal(&session).base(States::Without).unwrap().tracks.len(), 0);
}

#[test]
fn keeping_a_journal_can_be_switched_off_and_one_that_is_kept_can_be_deleted() {
    let (_root, journals, songs) = folders();
    let song = songs.join("song.trill");
    let mut session = session();
    session.keep_journals_in(journals.clone());
    let (track, ..) = bass(&mut session);
    session.say(Author::Person, "Something said.");
    session.save(&song).unwrap();
    let file = file_for(&journals, &song);
    assert!(file.exists() && session.keeps_journal());

    // Off: nothing is entered, calls are not opened, saving writes none.
    session.set_journal_keeping(false);
    assert!(session.journal().is_none() && !session.keeps_journal());
    session.edit(|p| p.set_track_volume(track, -3.0)).unwrap();
    session.say(Author::Person, "Not kept.");
    assert!(session.begin_call(Author::Assistant, "measure", "Measured the song").is_none());
    assert!(session.undo() && session.redo(), "undo is as ever");
    let before = read(&file);
    session.save(&song).unwrap();
    assert_eq!(read(&file), before, "what is on disk is left as it is");
    assert!(session.export_journal(&songs.join("x.trillstory"), false).is_err());

    // On again: the story is taken up, with a step for what it missed.
    session.set_journal_keeping(true);
    assert_eq!(last(&session).line(), "The song was changed while no journal was kept");
    assert!(lines(&session).contains(&"Something said.".to_owned()));
    assert!(!lines(&session).contains(&"Not kept.".to_owned()));
    assert_in_step(&session);
    // What can be undone was made before this journal: it is taken back
    // all the same, and the story says what it can.
    assert!(session.undo());
    assert_eq!(last(&session).line(), "Took back: an earlier step");
    assert_in_step(&session);

    // Deleted: the file is gone and the story begins here.
    session.delete_journal().unwrap();
    assert!(!file.exists());
    assert!(journal(&session).is_empty());
    assert_in_step(&session);
    session.edit(|p| p.set_track_pan(track, 0.5)).unwrap();
    session.save(&song).unwrap();
    assert!(!read(&file).contains("Something said."));
    // Deleting where there is nothing to delete is no error.
    session.new_project();
    session.delete_journal().unwrap();
}

#[test]
fn a_session_that_is_told_no_folder_writes_no_journal() {
    let (root, _journals, songs) = folders();
    let mut session = session();
    bass(&mut session);
    session.save(&songs.join("song.trill")).unwrap();
    session.load(&songs.join("song.trill")).unwrap();
    assert!(journal(&session).is_empty(), "nothing was kept to take up");
    let mut found = Vec::new();
    let mut folders = vec![root.path().to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(folder).unwrap().flatten() {
            match entry.path() {
                path if path.is_dir() => folders.push(path),
                path => found.push(path.file_name().unwrap().to_string_lossy().into_owned()),
            }
        }
    }
    assert_eq!(found, ["song.trill"]);
}

#[test]
fn a_take_is_one_step_and_the_journals_clock_can_be_set() {
    let mut session = session();
    let clock = Arc::new(AtomicU64::new(1_000));
    let reads = clock.clone();
    session.set_journal_clock(Arc::new(move || reads.load(Ordering::Relaxed)));
    let (track, ..) = bass(&mut session);
    clock.store(61_000, Ordering::Relaxed);
    session.edit(|p| p.set_track_volume(track, -3.0)).unwrap();
    let times: Vec<u64> = journal(&session).entries().map(|entry| entry.at).collect();
    assert_eq!(times, [1_000, 1_000, 1_000, 61_000]);
}

/// A song the size of a finished one: forty tracks, each with three
/// devices and ten clips of fifty notes, and a megabyte of plugin state
/// on every fourth track.
fn large(session: &mut Session) -> (Vec<TrackId>, Vec<ClipId>, Vec<DeviceId>) {
    let state: Vec<u8> = (0..1_000_000u32).map(|index| (index.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    let (mut tracks, mut clips, mut devices) = (Vec::new(), Vec::new(), Vec::new());
    session
        .edit(|p| {
            for index in 0..40 {
                let track = p.add_track(format!("Track {index}"));
                tracks.push(track);
                for (uri, name) in
                    [("builtin:polysynth", "Poly Synth"), ("builtin:eq", "EQ"), ("builtin:reverb", "Reverb")]
                {
                    devices.push(p.add_device(ChainRef::Track(track), uri, name, None)?);
                }
                if index % 4 == 0 {
                    let device = p.add_device(ChainRef::Track(track), "clap:not-installed", "Big Sampler", None)?;
                    p.device_mut(device)?.state = Some(state.clone());
                }
                for part in 0..10 {
                    let clip = p.add_midi_clip(track, format!("Part {part}"), part as f64 * 16.0, 16.0)?;
                    let notes: Vec<Note> =
                        (0..50).map(|step| note_at(step as f64 * 0.25, 36 + (step % 48) as u8)).collect();
                    p.clip_mut(clip)?.add_notes(&notes)?;
                    clips.push(clip);
                }
            }
            Ok(())
        })
        .unwrap();
    (tracks, clips, devices)
}

/// What recording a step costs an edit, on a large project. Run it alone,
/// on a machine that is not busy:
///
/// ```sh
/// cargo test -p trill-engine --test journal what_recording_a_step_costs -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a measurement: prints what recording a step costs on a large project"]
fn what_recording_a_step_costs_an_edit_on_a_large_project() {
    fn edits(
        session: &mut Session,
        tracks: &[TrackId],
        clips: &[ClipId],
        devices: &[DeviceId],
        rounds: usize,
    ) -> Duration {
        let started = Instant::now();
        for round in 0..rounds {
            let track = tracks[round % tracks.len()];
            let clip = clips[(round * 7) % clips.len()];
            let device = devices[(round * 3) % devices.len()];
            session.edit(|p| p.set_track_volume(track, -((round % 40) as f32) / 4.0)).unwrap();
            session.edit(|p| p.clip_mut(clip)?.add_notes(&[note_at(12.0 + (round % 16) as f64 * 0.25, 60)])).unwrap();
            session
                .edit(|p| {
                    p.device_mut(device)?.enabled = round % 2 == 0;
                    Ok(())
                })
                .unwrap();
        }
        started.elapsed() / (rounds as u32 * 3)
    }

    let rounds = 60;
    let mut kept = session();
    let (tracks, clips, devices) = large(&mut kept);
    let project_bytes = kept.project().to_json().len();
    let with = edits(&mut kept, &tracks, &clips, &devices, rounds);
    assert_in_step(&kept);

    let mut bare = session();
    bare.set_journal_keeping(false);
    let (tracks, clips, devices) = large(&mut bare);
    let without = edits(&mut bare, &tracks, &clips, &devices, rounds);

    // The journal's own part, measured apart from the engine's: reading
    // what differs between two states of the song.
    let before = kept.project().clone();
    let mut after = before.clone();
    after.set_track_volume(tracks[3], -2.5).unwrap();
    let mut journal = Journal::begin(&before, 0);
    let started = Instant::now();
    let reads = 200;
    for index in 0..reads {
        let (from, to) = if index % 2 == 0 { (&before, &after) } else { (&after, &before) };
        journal.record(index, trill_core::journal::Who::Person, from, to, None);
    }
    let read = started.elapsed() / reads as u32;
    let story = self::journal(&kept).to_text(true).len();
    println!(
        "a project of {:.1} MB ({} tracks, {} clips, {} notes): an edit takes {with:?} with the journal and \
         {without:?} without; reading what an edit changed takes {read:?}; {} steps are {} bytes of journal beyond \
         the song it began with",
        project_bytes as f64 / 1e6,
        tracks.len(),
        clips.len(),
        clips.len() * 50,
        self::journal(&kept).len() - 1,
        story - Journal::begin(&large_base(&kept), 0).to_text(true).len(),
    );
    assert!(read < Duration::from_millis(50), "reading what an edit changed took {read:?}");
}

/// The song as the large session's journal had it after its first entry:
/// what the edits measured above were made on.
fn large_base(session: &Session) -> Project {
    let journal = journal(session);
    journal.song_after(1, States::With(journal.blobs())).unwrap()
}
