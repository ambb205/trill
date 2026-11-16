//! The Mixer tab.

use super::*;
use trill_core::TrackKind;

fn mixer_tab() -> UiOptions {
    UiOptions { detail_tab: DetailTab::Mixer, ..Default::default() }
}

#[test]
fn the_mixer_has_a_strip_for_every_track_and_the_master() {
    let mut ids = None;
    let mut harness = Harness::new(mixer_tab(), |session| {
        let (first, second, _) = two_tracks(session);
        let synth = session.add_device(ChainRef::Track(first), "builtin:polysynth", None, None).unwrap();
        ids = Some((first, second, synth));
    });
    let (first, second, synth) = ids.unwrap();
    for name in ["Volume of One", "Volume of Two", "Master volume"] {
        assert!(harness.is_on_screen(name), "a fader named '{name}'");
    }

    // The fader is the volume: the pointer sets it, as one undo step.
    let before = harness.undo_depth();
    let fader = harness.all_labeled("Volume of One")[0];
    harness.drag(fader.center_top() + vec2(0.0, 12.0), fader.center());
    let volume = harness.project().track(first).unwrap().volume_db;
    assert!((-18.5..-14.5).contains(&volume), "half way up the fader scale: {volume}");
    assert_eq!(harness.undo_depth(), before + 1);
    harness.double_click(fader.center());
    assert_eq!(harness.project().track(first).unwrap().volume_db, 0.0, "a double-click returns to unity");

    let master = harness.all_labeled("Master volume")[0];
    harness.drag(master.center_top() + vec2(0.0, 12.0), master.center());
    assert!(harness.project().master.volume_db < -10.0);

    harness.click_label("Mute Two in the mixer");
    harness.click_label("Solo One in the mixer");
    assert!(harness.project().track(second).unwrap().mute && harness.project().track(first).unwrap().solo);

    // The strips are laid out to one plan, so their controls line up
    // whatever each holds: the first track has a device, the second none.
    let pans: Vec<Rect> =
        harness.all_labeled("Pan").into_iter().filter(|rect| rect.top() > harness.detail_top()).collect();
    assert_eq!(pans.len(), 2);
    assert_eq!(pans[0].top(), pans[1].top());
    let faders = [harness.all_labeled("Volume of One")[0], harness.all_labeled("Volume of Two")[0], master];
    assert!(faders.iter().all(|fader| fader.y_range() == faders[0].y_range()), "{faders:?}");

    // The strip's own pan cell.
    harness.settle();
    let pan = harness.all_labeled("Pan").into_iter().find(|rect| rect.top() > harness.detail_top()).unwrap();
    harness.click(pan.left_center() + vec2(pan.width() * 0.75, 0.0));
    assert!((harness.project().track(first).unwrap().pan - 0.5).abs() < 0.06);

    // A device in a strip opens it in the Devices tab.
    harness.click_label("Poly Synth in the mixer");
    assert_eq!(harness.app.state().detail_tab, DetailTab::Devices);
    assert_eq!(harness.app.state().selected_device, Some(synth));
    assert_eq!(harness.app.state().selected_track, Some(first));

    harness.click_label("Mixer");
    harness.click_label("Limiter in the mixer");
    assert_eq!(harness.app.state().shown_chain(), ChainRef::Master, "the master's devices are in its strip too");
}

#[test]
fn sends_are_set_from_the_strips() {
    let mut ids = None;
    let mut harness = Harness::new(mixer_tab(), |session| {
        let (first, ..) = two_tracks(session);
        let returns = session.edit(|project| Ok(project.add_track_of_kind("Space", TrackKind::Return))).unwrap();
        ids = Some((first, returns));
    });
    let (first, returns) = ids.unwrap();
    // One send bar per track that can send; the return itself has none.
    let sends = harness.all_labeled("Space");
    assert_eq!(sends.len(), 2);
    let before = harness.undo_depth();
    harness.drag(sends[0].left_center() + vec2(8.0, 0.0), sends[0].left_center() + vec2(sends[0].width() * 0.85, 0.0));
    let sent = harness.project().track(first).unwrap().sends.clone();
    assert_eq!((sent.len(), sent[0].to), (1, returns));
    assert!((sent[0].level_db).abs() < 1.5, "85% of the way along is unity: {}", sent[0].level_db);
    assert_eq!(harness.undo_depth(), before + 1);
}

#[test]
fn a_strips_fader_takes_hold_of_an_automated_volume() {
    let mut first = None;
    let mut harness = Harness::new(mixer_tab(), |session| first = Some(two_tracks(session).0));
    let first = first.unwrap();
    let target = trill_core::AutomationTarget::TrackVolume { track: first };
    let points = vec![trill_core::AutomationPoint::new(0.0, -20.0)];
    harness.session.lock().edit(|project| project.set_automation(target.clone(), points.clone())).unwrap();
    harness.idle(2);
    let lane_on = |harness: &Harness| harness.project().automation[0].enabled;
    let volume = |harness: &Harness| harness.project().track(first).unwrap().volume_db;
    let off_mark = "Automation of Volume of One is off";

    // The fader shows what the envelope says, not the volume the track
    // was left at.
    let at_minus_20 = crate::ui::units::db_to_fader(-20.0) as f64;
    assert!((harness.shown("Volume of One") - at_minus_20).abs() < 0.01, "{}", harness.shown("Volume of One"));
    assert_eq!(volume(&harness), 0.0);
    assert!(!harness.is_on_screen(off_mark));

    // Dragged, it is the user's: the envelope is switched off, kept, and
    // the fader stays where it is put.
    let before = harness.undo_depth();
    let fader = harness.all_labeled("Volume of One")[0];
    harness.drag(fader.center(), fader.center_top() + vec2(0.0, 6.0));
    assert!(volume(&harness) > 3.0, "{}", volume(&harness));
    assert!(!lane_on(&harness));
    assert_eq!(harness.project().automation[0].points, points, "the envelope itself is untouched");
    assert_eq!(harness.undo_depth(), before + 1, "taking hold and the drag are one undo step");
    assert!(harness.shown("Volume of One") > 0.9, "the fader shows the volume it was given");
    assert!(harness.app.state().arrangement.lanes.is_empty(), "nothing is unfolded behind the user's back");

    // The mark in the fader's corner is the way back, in one click.
    harness.settle();
    harness.click_label(off_mark);
    assert!(lane_on(&harness));
    assert!(volume(&harness) > 3.0, "the volume set by hand is kept for the next time");
    harness.idle(1);
    assert!((harness.shown("Volume of One") - at_minus_20).abs() < 0.01, "the envelope sets the fader again");
    assert!(!harness.is_on_screen(off_mark));

    // Undo walks back through it: off again, then as it was at the start.
    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(!lane_on(&harness));
    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(lane_on(&harness));
    assert_eq!(volume(&harness), 0.0, "one step gives back both the envelope and the volume");
    assert_eq!(harness.undo_depth(), before);
}

#[test]
fn thirty_tracks_scroll_sideways_at_a_usable_width() {
    let mut harness = Harness::new(mixer_tab(), |session| {
        session
            .edit(|project| {
                for index in 0..34 {
                    project.add_track(format!("T{index}"));
                }
                Ok(())
            })
            .unwrap();
    });
    let strips: Vec<Rect> = (0..34)
        .filter_map(|index| harness.all_labeled(&format!("Mute T{index} in the mixer")).first().copied())
        .collect();
    assert!(strips.len() >= 8 && strips.len() < 34, "only the strips in view are built: {}", strips.len());
    let pitch = strips[1].left() - strips[0].left();
    assert!(pitch >= 80.0, "each strip keeps a width its controls fit in: {pitch}");
    assert!(harness.is_on_screen("Master volume"), "the master stays pinned at the right");
    assert!(!harness.is_on_screen("Volume of T33"));

    // The wheel brings the rest into view.
    harness.wheel(Modifiers::SHIFT, strips[2].center(), -400.0);
    harness.idle(2);
    assert!(harness.is_on_screen("Volume of T33"), "scrolled to the last track");
    assert!(!harness.is_on_screen("Volume of T0"));
    assert!(harness.is_on_screen("Master volume"));
}
