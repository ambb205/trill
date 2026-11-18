//! The playback load: how the machine keeps up with the song, what each
//! track costs, and what the window says of a song that is more than the
//! machine plays in time. The loads are made up (`LoadScene`): how busy
//! the machine is while a test runs is not what is tested.

use super::*;
use crate::ui::LoadScene;

fn with_load(scene: LoadScene) -> Harness {
    let options = UiOptions { load_scene: Some(scene), ..Default::default() };
    let mut harness = Harness::new(options, |session| {
        two_tracks(session);
    });
    harness.idle(3);
    harness
}

#[test]
fn a_click_on_the_cpu_readout_says_how_the_song_is_played_and_what_each_track_costs() {
    let mut harness = with_load(LoadScene::Calm);
    assert!(!harness.app.state().load_open);
    harness.click_label("Playback load");
    harness.idle(2);
    assert!(harness.app.state().load_open);
    for said in [
        "Audio thread: 37 % of its time on average, 74 % for the worst buffer",
        "Late buffers: None of 1,000 in the last 10 s",
        "Threads: 8 of the 8 it may have",
        // The costliest first, each with what it takes of a processor.
        "One: 25.9 % of a processor",
        "Two: 3.1 % of a processor",
        "All the tracks: 29.0 %",
    ] {
        assert!(harness.is_on_screen(said), "the panel does not say: {said}");
    }
    let (one, two) = (harness.labeled("One: 25.9 % of a processor"), harness.labeled("Two: 3.1 % of a processor"));
    assert!(one.y < two.y, "the costlier track is above the other");
    // A song that is played in time is not spoken of anywhere else.
    assert_eq!(harness.app.state().toasts.len(), 0);
    harness.key(Modifiers::NONE, Key::Escape);
    harness.idle(2);
    assert!(!harness.app.state().load_open, "Escape closes it");
}

#[test]
fn a_song_that_is_more_than_the_machine_plays_in_time_is_said_so_once_and_stays_said() {
    let mut harness = with_load(LoadScene::Short);
    // Many frames have gone by with the song falling short: one notice.
    harness.idle(40);
    let notices: Vec<String> = harness.app.state().toasts.texts().iter().map(|text| text.to_string()).collect();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].starts_with("This song is more than this machine plays in time: buffers are coming late"));
    assert!(notices[0].contains("An export is not affected."), "{notices:?}");

    // The status bar says it for as long as it holds, and leads to the rest.
    let line = "This song is more than this machine plays in time \u{00B7} 647 buffers late";
    assert!(harness.is_on_screen(line));
    harness.click_label(line);
    harness.idle(2);
    assert!(harness.app.state().load_open);
    let sentence = LoadScene::Short.of(&harness.project()).short.expect("the scene falls short").sentence();
    assert!(sentence.contains("What costs most: One (106 % of a processor)"), "{sentence}");
    assert!(harness.is_on_screen(&sentence), "the panel has the whole sentence");
    assert!(harness.is_on_screen("Late buffers: 647 of 1,000 in the last 10 s"));
    assert!(harness.is_on_screen("One: 106.2 % of a processor"));
    harness.idle(20);
    assert_eq!(harness.app.state().toasts.len(), 1, "and still one notice");
}
