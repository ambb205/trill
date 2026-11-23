//! Two windows in one song, one of them on a computer that has not got
//! the song's instrument: its track is heard there as the other recorded
//! it, and the window says so where the track is.
//!
//! The instrument is the engine's scripted bass, which only one of the
//! two runtimes is handed: a plugin that is installed on one computer and
//! not on the other, as far as anyone is concerned. Each window is on a
//! runtime of its own, and the two meet over this machine's loopback.

use super::*;
use trill_core::merge::lot::note;
use trill_engine::scripted::{BASS, ScriptedFactory};
use trill_runtime::{Parts, Reach};

/// Runs both windows until `done`.
fn both_until(a: &mut Harness, b: &mut Harness, what: &str, done: impl Fn(&Harness, &Harness) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(a, b) {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(5));
        a.idle(1);
        b.idle(1);
    }
}

/// 1 connection.
#[test]
fn a_track_whose_plugin_is_on_the_others_computer_says_that_it_is_heard_as_they_recorded_it() {
    // Ada's computer has the bass; Ben's has not.
    let parts = Parts { devices: vec![Box::new(ScriptedFactory::new())], ..Parts::default() };
    let with_it = Arc::new(Runtime::start_custom(RuntimeOptions::hermetic(), parts).unwrap());
    let mut ids = None;
    let mut ada = Harness::on(with_it, UiOptions::default(), |session| {
        let bass = session.edit(|song| Ok(song.add_track("Bass"))).unwrap();
        session.edit(|song| Ok(song.add_track("Keys"))).unwrap();
        session.add_device(ChainRef::Track(bass), BASS, None, None).unwrap();
        let line = session.edit(|song| song.add_midi_clip(bass, "Line", 0.0, 8.0)).unwrap();
        session.edit(|song| song.clip_mut(line)?.add_notes(&[note(40, 0.0), note(43, 2.0)])).unwrap();
        ids = Some((bass, line));
    });
    let (bass, line) = ids.unwrap();
    ada.runtime.set_name_together("Ada");
    let mut ben = Harness::new(UiOptions::default(), |_| {});
    ben.runtime.set_name_together("Ben");
    let recorded = "Bass: heard as Ada\u{2019}s Trill recorded it";
    assert!(!ada.is_on_screen(recorded));

    let invitation = ada.runtime.share(Reach::ThisMachine, 0).unwrap();
    ben.runtime.join(&invitation, true).unwrap();
    both_until(&mut ada, &mut ben, "Ben has the song", |_, ben| ben.project().tracks.len() == 2);

    // Ada's Trill records the bass for him, by itself; his window says
    // where the track is that he hears it so, once it is there. (While
    // hers records, its status bar says which track: a moment, for a song
    // this short, and what it says is held where the words are made.)
    both_until(&mut ada, &mut ben, "Ben's window says how he hears the bass", |_, ben| ben.is_on_screen(recorded));
    assert!(!ada.is_on_screen(recorded), "Ada has the plugin, and nothing is said to her");
    both_until(&mut ada, &mut ben, "Ada's Trill is done", |ada, _| {
        ada.app.live().together.recording_for_others.is_none()
    });
    let state = ben.session.lock().renditions().first().cloned().expect("Ben's song has the recording");
    assert_eq!((state.track, state.played, state.as_the_song_is()), (bass, true, true));

    // He writes a note into the bass line: what he hears is the bass as
    // it was, which his window says at once, until Ada's has recorded it
    // again.
    ben.session.lock().edit(|song| song.clip_mut(line)?.add_notes(&[note(47, 4.0)])).unwrap();
    ben.idle(2);
    let as_it_was = "Bass: heard as Ada\u{2019}s Trill recorded it before the last change";
    assert!(ben.is_on_screen(as_it_was));
    both_until(&mut ada, &mut ben, "the bass is recorded again and Ben hears the note", |_, ben| {
        !ben.is_on_screen(as_it_was) && ben.is_on_screen(recorded)
    });
    assert!(ben.session.lock().renditions()[0].as_the_song_is());
}
