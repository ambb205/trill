//! The wavetable synth's panel: the wave an oscillator reads, its macros
//! under the names the sound gives them, and its matrix as a list.

use super::*;

/// A song of one track with a wavetable synth on it, from `preset`.
fn with_synth(preset: &str) -> (Harness, DeviceId) {
    let mut device = None;
    let harness = Harness::new(devices_tab(), |session| {
        let track = session.edit(|project| Ok(project.add_track("Lead"))).unwrap();
        device = Some(session.add_device(ChainRef::Track(track), "builtin:wavetable", Some(preset), None).unwrap());
    });
    (harness, device.unwrap())
}

#[test]
fn the_panel_shows_the_wave_an_oscillator_reads_and_follows_its_table_and_position() {
    let (mut harness, device) = with_synth("Sync Lead");
    harness.idle(3);
    let value = |harness: &Harness, id: &str| harness.project().device(device).unwrap().params.get(id).copied();
    // What the picture shows is said in words as well.
    assert!(harness.is_on_screen("Oscillator 1 reads Sync Sweep: 32 frames, here at 0.25"));
    assert!(harness.is_on_screen("Table: Sync Sweep"));

    // Another table from the menu: one undo step, and the picture is of it.
    let before = harness.undo_depth();
    harness.click_label("Table: Sync Sweep");
    harness.click_label("Acid");
    harness.idle(2);
    assert_eq!(value(&harness, "osc1_table"), Some(7.0));
    assert_eq!(harness.undo_depth(), before + 1);
    assert!(harness.is_on_screen("Oscillator 1 reads Acid: 24 frames, here at 0.25"));

    // The position moved from elsewhere (as a tool call moves it) moves
    // the picture.
    harness.session.lock().set_device_params(device, &[("osc1_position".into(), 0.8)]).unwrap();
    harness.idle(2);
    assert!(harness.is_on_screen("Oscillator 1 reads Acid: 24 frames, here at 0.80"));

    // The second oscillator has a page of its own.
    harness.click_label("Osc 2");
    harness.idle(2);
    assert!(harness.is_on_screen("Oscillator 2 reads Basic Shapes: 5 frames, here at 0.50"));
    // A table of its own, made from a recipe elsewhere, is drawn once it
    // has been made for the picture.
    let recipe = trill_dsp::wavetable::recipe::Recipe::new(
        "Thin Reed",
        12,
        vec![trill_dsp::wavetable::recipe::Frame::of(trill_dsp::wavetable::recipe::Shape::Square)],
    );
    let own = trill_dsp::wavetable::TableSource::Recipe(recipe);
    harness.session.lock().set_wavetable(device, 2, own).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !harness.is_on_screen("Oscillator 2 reads Thin Reed: 12 frames, here at 0.50") {
        assert!(Instant::now() < deadline, "the oscillator's own table was never drawn");
        std::thread::sleep(Duration::from_millis(5));
        harness.idle(1);
    }
    assert!(harness.is_on_screen("Table: custom"));
}

#[test]
fn the_macros_are_shown_under_their_names_and_the_matrix_as_a_list() {
    let (mut harness, device) = with_synth("Sync Lead");
    harness.idle(3);
    let value = |harness: &Harness, id: &str| harness.project().device(device).unwrap().params.get(id).copied();
    // The preset's names for its macros, each a bar.
    for name in ["Rip", "Vibrato", "Bright", "Body"] {
        assert!(harness.is_on_screen(name), "no macro is shown as {name}");
    }
    assert_eq!(harness.shown("Rip"), 0.0);
    harness.session.lock().set_device_params(device, &[("macro1".into(), 0.5)]).unwrap();
    harness.idle(2);
    assert!((harness.shown("Rip") - 0.5).abs() < 0.01);
    // A name given elsewhere is the bar's name here.
    harness.session.lock().name_macros(device, &[(1, "Tear".into())]).unwrap();
    harness.idle(2);
    assert!(harness.is_on_screen("Tear") && !harness.is_on_screen("Rip"));

    // The first slot of the matrix: the second envelope moves the first
    // oscillator's position. What it reads is chosen from a menu.
    assert!(harness.is_on_screen("env2") && harness.is_on_screen("osc1_position"));
    assert_eq!(value(&harness, "mod1_source"), Some(2.0));
    harness.click_label("env2");
    harness.click_label("velocity");
    harness.idle(2);
    assert_eq!(value(&harness, "mod1_source"), Some(7.0));

    // Every page of envelopes and LFOs is there to turn to.
    for page in ["Level", "Env 2", "Env 3", "LFO 1", "LFO 2", "LFO 3", "Filter 1", "Filter 2"] {
        assert!(harness.is_on_screen(page), "no page {page}");
    }
    harness.click_label("LFO 2");
    harness.idle(2);
    assert!(harness.is_on_screen("Shape: sine"));
}
