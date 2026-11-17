//! A film of the song, from the window: the panel and its choices, and on
//! a machine with a graphics card the film itself.
//!
//! Where a film goes is asked by the system's own dialog, which nothing
//! here can answer; the tests that make a film hand the place over where
//! the dialog's answer would arrive.

use super::*;
use crate::ui::filming::{Draft, Of, Stretch};

const CTRL_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);

/// Three bars: two tracks with a clip at bar 3, an intro of two bars and a
/// verse of one.
fn with_sections() -> Harness {
    Harness::new(UiOptions::default(), |session| {
        two_tracks(session);
        let mark = |project: &mut Project| {
            project.add_marker(0.0, "Intro")?;
            project.add_marker(8.0, "Verse")?;
            Ok(())
        };
        session.edit(mark).unwrap();
    })
}

/// The line of the panel that begins with `start`.
fn line(harness: &Harness, start: &str) -> Option<String> {
    harness.labels.iter().map(|(text, _)| text.clone()).find(|text| text.starts_with(start))
}

#[test]
fn the_film_panel_opens_from_the_file_menu_and_says_what_its_choices_come_to() {
    let mut harness = with_sections();
    harness.click_label("File");
    harness.click_label("Export a film");
    harness.idle(3);
    assert!(harness.app.filming().is_open());
    for choice in [
        "The song",
        "How it was made",
        "1920 \u{00D7} 1080",
        "3840 \u{00D7} 2160",
        "1080 \u{00D7} 1920 upright",
        "60 a second",
        "Fine",
        "Plain",
        "Whole song",
        "Intro",
        "Verse",
        "Choose where\u{2026}",
        "Cancel",
    ] {
        assert!(harness.is_on_screen(choice), "{choice} is offered");
    }
    assert!(!harness.is_on_screen("Selection"), "no time is selected, so none is offered");
    assert!(harness.is_on("The song") && harness.is_on("Fine") && harness.is_on("Whole song"));
    assert_eq!(line(&harness, "Bars").as_deref(), Some("Bars 1 to 3 \u{00B7} 0:06 \u{00B7} 360 frames"));

    // Each choice is one click, and the line under them follows.
    harness.click_label("3840 \u{00D7} 2160");
    harness.click_label("24 a second");
    harness.click_label("Plain");
    harness.click_last_label("Verse");
    harness.idle(3);
    let draft = *harness.app.filming().draft();
    assert_eq!((draft.of, draft.size, draft.rate, draft.fine), (Of::Song, 1, 2, false));
    assert_eq!(draft.stretch, Stretch::Section(1));
    assert!(harness.is_on("Verse") && !harness.is_on("Whole song") && harness.is_on("Plain"));
    assert_eq!(line(&harness, "Bar ").as_deref(), Some("Bar 3 \u{00B7} 0:02 \u{00B7} 48 frames"));

    // How it was made is filmed whole: there is no stretch to choose.
    harness.click_label("How it was made");
    harness.idle(3);
    assert_eq!(harness.app.filming().draft().of, Of::Making);
    assert!(!harness.is_on_screen("Whole song") && !harness.is_on_screen("Intro"));
    let made = harness.labels.iter().map(|(text, _)| text.as_str()).find(|text| text.contains("of the journal"));
    let made = made.expect("what a film of the making comes to");
    assert!(made.starts_with("2 steps of the journal"), "the two steps the song was made in: {made}");
    assert!(made.contains("the finished song plays under it"), "{made}");

    // Escape takes the panel down; its key puts it up as it was left.
    harness.key(Modifiers::NONE, Key::Escape);
    harness.idle(3);
    assert!(!harness.app.filming().is_open() && !harness.is_on_screen("Choose where\u{2026}"));
    harness.key(CTRL_SHIFT, Key::F);
    harness.idle(3);
    assert!(harness.app.filming().is_open());
    assert_eq!(*harness.app.filming().draft(), Draft { of: Of::Making, ..draft });
    harness.click_label("Cancel");
    harness.idle(3);
    assert!(!harness.app.filming().is_open());
    // Counted last: the harness counts steps by undoing them, which the journal keeps too.
    assert_eq!(harness.undo_depth(), 2, "choosing a film edits nothing");
}

#[test]
fn selected_time_is_a_stretch_a_film_can_be_of_and_the_whole_song_once_it_is_gone() {
    let mut harness = with_sections();
    // Bar 2, selected along the foot of the ruler.
    harness.drag(harness.ruler(4.2, false), harness.ruler(7.9, false));
    harness.key(CTRL_SHIFT, Key::F);
    harness.idle(3);
    harness.click_label("Selection");
    harness.idle(3);
    assert_eq!(harness.app.filming().draft().stretch, Stretch::Selection);
    assert_eq!(line(&harness, "Bar ").as_deref(), Some("Bar 2 \u{00B7} 0:02 \u{00B7} 120 frames"));

    // The selection is dropped behind the panel's back: the film is of
    // the whole song again, and says so.
    harness.app.state_mut().arrangement.time_selection = None;
    harness.idle(3);
    assert!(!harness.is_on_screen("Selection"));
    assert_eq!(harness.app.filming().draft().stretch, Stretch::Whole);
    assert!(line(&harness, "Bars 1 to 3").is_some());
}

#[test]
fn an_empty_song_has_no_film_to_choose_a_place_for() {
    let mut harness = Harness::new(UiOptions::default(), |_| {});
    harness.key(CTRL_SHIFT, Key::F);
    harness.idle(3);
    assert!(harness.app.filming().is_open());
    assert!(!harness.is_on_screen("How it was made"), "nothing was made yet");
    assert!(line(&harness, "The song is empty").is_some());
    // The button is there and does nothing.
    harness.click_label("Choose where\u{2026}");
    harness.idle(3);
    assert!(harness.app.filming().is_open() && harness.app.filming().making().is_none());
}

#[test]
fn the_making_of_leads_to_its_own_film() {
    let mut harness = with_sections();
    harness.key(CTRL_SHIFT, Key::M);
    harness.idle(3);
    assert!(harness.app.making_of().is_some());
    harness.wait_until("the making-of's Film button", |harness| harness.is_on_screen("Film\u{2026}"));
    harness.click_label("Film\u{2026}");
    harness.idle(4);
    assert!(harness.app.making_of().is_none(), "the film's panel is the song's own window's");
    assert!(harness.app.filming().is_open());
    assert_eq!(harness.app.filming().draft().of, Of::Making);
    assert!(harness.is_on("How it was made"));
}

/// Waits for a film that was begun to end, looking at the window as it
/// does, and returns what the window said of it.
fn film_through(harness: &mut Harness) -> String {
    let before = harness.app.state().toasts.len();
    let began = Instant::now();
    let mut seen_progress = false;
    while harness.app.state().toasts.len() == before {
        assert!(began.elapsed() < Duration::from_secs(240), "the film did not end");
        std::thread::sleep(Duration::from_millis(20));
        harness.idle(1);
        seen_progress |= line(harness, "Frame ").is_some() && harness.is_on_screen("Stop the film");
    }
    assert!(seen_progress, "the panel said how far the film was, with a way to stop it");
    harness.app.state().toasts.texts().last().map(|text| (*text).to_owned()).unwrap()
}

#[test]
#[ignore = "needs a graphics card; run with `cargo test -p trill-app a_film_is_made_from_the_window -- --ignored --nocapture`"]
fn a_film_is_made_from_the_window_while_it_says_how_far_it_is_and_one_can_be_stopped() {
    let folder = tempfile::tempdir().unwrap();
    // Three bars that sound: the first track plays through a synth.
    let mut harness = Harness::new(UiOptions::default(), |session| {
        let (first, ..) = two_tracks(session);
        session.add_device(ChainRef::Track(first), "builtin:polysynth", None, None).unwrap();
    });
    harness.key(CTRL_SHIFT, Key::F);
    harness.idle(3);
    harness.click_label("24 a second");
    harness.click_label("Plain");
    harness.idle(2);

    // Made: the whole of three bars, with the song's own sound.
    let path = folder.path().join("three bars.mp4");
    harness.app.film_to(&harness.ctx.clone(), path.clone());
    let said = film_through(&mut harness);
    println!("{said}");
    assert!(said.starts_with("Filmed three bars.mp4: 0:0"), "{said}");
    assert!(said.contains("LUFS"), "its own sound was rendered and measured: {said}");
    assert!(!harness.app.filming().is_open() && harness.app.filming().making().is_none());
    // One file where ffmpeg is, numbered pictures with the sound where it is not.
    let frames = path.with_extension("frames");
    assert!(path.is_file() || frames.is_dir(), "{said}");
    assert!(path.is_file() || said.contains("ffmpeg was not found"), "{said}");
    assert!(!path.with_extension("sound.wav").exists(), "what it was put together from is not left behind");
    let song = harness.project();

    // Stopped: nothing of it is kept, and that is news, not a failure.
    let path = folder.path().join("stopped.mp4");
    harness.app.film_to(&harness.ctx.clone(), path.clone());
    harness.wait_until("the film to begin", |harness| harness.app.filming().making().is_some());
    let named = harness.app.filming().making().map(|(file, _)| file.to_path_buf());
    assert_eq!(named, Some(path.clone()), "the status bar names the film");
    harness.wait_until("its frames", |harness| line(harness, "Frame ").is_some());
    harness.click_label("Stop the film");
    harness.wait_until("the film to stop", |harness| harness.app.filming().making().is_none());
    let said = harness.app.state().toasts.texts().last().map(|text| (*text).to_owned()).unwrap();
    assert!(said.starts_with("The film was stopped at frame"), "{said}");
    assert!(!path.exists() && !path.with_extension("sound.wav").exists(), "nothing of it was kept");
    assert_eq!(harness.project(), song, "and a film edits nothing");
}
