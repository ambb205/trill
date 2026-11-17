//! Sounds of more than one device in the window: saved from a track's
//! instrument with what they say of themselves, listed in the browser as
//! sounds, made a track's by a double-click or by being dragged there,
//! and renamed, described and deleted from where they are listed.

use super::*;
use crate::ui::sound_presets::{FormFor, Target};

const WAVETABLE: &str = "builtin:wavetable";
const SYNTH: &str = "builtin:polysynth";
const REVERB: &str = "builtin:reverb";

/// The devices of the `row`th track, by name.
fn chain(harness: &Harness, row: usize) -> Vec<String> {
    harness.project().tracks[row].devices.iter().map(|device| device.name.clone()).collect()
}

impl Harness {
    /// Presses on the browser's row of the sound called `name` and carries
    /// it to `to` without letting go.
    fn carry_preset(&mut self, name: &str, to: Pos2) {
        self.idle(2);
        let from = self.labeled(&format!("Sound {name}"));
        self.drag_start(Modifiers::NONE, from, to);
        self.idle(1);
    }

    /// Opens, or closes, the rows of the sounds of `kind` that a built-in
    /// instrument brings: they are one row until then.
    fn toggle_kind(&mut self, kind: &str) {
        self.click_label(&format!("Sounds of the kind {kind}"));
        self.settle();
    }

    /// Puts the keyboard in the field of the sound form that is captioned
    /// `caption`, with what is in it selected.
    fn form_field(&mut self, caption: &str) {
        self.idle(2);
        let field = self.labeled(&format!("Sound {caption}")) + vec2(0.0, 20.0);
        self.click(field);
        self.key(Modifiers::COMMAND, Key::A);
    }

    /// The menu of the browser's row of what was saved as `name`.
    fn saved_menu(&mut self, name: &str) {
        self.idle(2);
        let row = self.labeled(&format!("Sound {name}"));
        self.right_click(row);
        self.idle(3);
    }
}

#[test]
fn a_tracks_sound_is_saved_with_what_it_says_of_itself_and_is_a_sound_in_the_browser() {
    let mut harness = Harness::new(devices_tab(), |_| {});
    harness.key(Modifiers::COMMAND, Key::T);
    harness.idle(2);
    let track = harness.project().tracks[0].id;
    let (synth, reverb) = {
        let mut session = harness.session.lock();
        let synth = session.project().tracks[0].devices[0].id;
        session.load_preset(synth, "warm_pad").unwrap();
        let reverb = session.add_device(ChainRef::Track(track), REVERB, None, None).unwrap();
        session.set_device_params(reverb, &[("decay".into(), 6.0)]).unwrap();
        (synth, reverb)
    };
    harness.idle(2);
    assert!(!harness.is_on_screen("Sound Warm Hall"));

    // The instrument's menu offers to save the track's sound; the effect
    // behind it does not, since no sound begins there.
    harness.click_last_label("Preset");
    harness.idle(3);
    assert!(!harness.is_on_screen("Save sound\u{2026}"), "the reverb's menu");
    harness.key(Modifiers::NONE, Key::Escape);
    harness.idle(2);
    // (The synth's menu is the first thing called after its preset; the
    // browser lists the preset too, further down.)
    harness.click_label("warm_pad");
    harness.click_label("Save sound\u{2026}");
    harness.idle(2);
    let form = harness.app.state().preset_form.clone().expect("the form is up");
    assert_eq!((form.what, form.name.as_str()), (FormFor::Save { track }, ""));

    // Without a name nothing is saved: it is said, and the form stays.
    harness.key(Modifiers::NONE, Key::Enter);
    harness.idle(2);
    assert!(harness.app.state().preset_form.is_some());
    assert!(harness.session.lock().saved_presets().is_empty());

    // A name, what it is, and words to find it by.
    harness.type_text("Warm Hall");
    harness.form_field("Description");
    harness.type_text("A soft pad in a long hall");
    harness.form_field("Tags");
    harness.type_text("pad, warm , hall");
    harness.key(Modifiers::NONE, Key::Enter);
    harness.idle(3);
    assert!(harness.app.state().preset_form.is_none());

    // One sound, of the synth, with the reverb as it was set.
    let sounds = harness.session.lock().saved_presets().sounds();
    assert_eq!(sounds.len(), 1);
    assert_eq!((sounds[0].uri.as_str(), sounds[0].info.name.as_str()), (SYNTH, "Warm Hall"));
    assert_eq!(sounds[0].after, ["Reverb"]);
    assert_eq!(sounds[0].info.description, "A soft pad in a long hall");
    assert_eq!(sounds[0].info.tags, ["pad", "warm", "hall"]);
    let project = harness.project();
    assert_eq!(project.device(synth).unwrap().preset.as_ref().unwrap().name, "Warm Hall");
    assert!(project.device(reverb).unwrap().preset.as_ref().unwrap().bringer().is_some());
    // The browser lists it as a sound, ahead of the kinds a built-in
    // instrument brings, which are a row each until one is opened.
    assert!(harness.is_on_screen("Sound Warm Hall") && !harness.is_on_screen("Sound Glass Pad"));
    harness.toggle_kind("Pad");
    assert!(harness.is_on_screen("Sound Glass Pad") && harness.is_on_screen("Sound Warm Pad"));
    harness.toggle_kind("Pad");
    assert!(!harness.is_on_screen("Sound Glass Pad"));

    // Given to another track from the browser, the reverb comes with it.
    harness.key(Modifiers::COMMAND, Key::T);
    harness.idle(2);
    let undo = harness.undo_depth();
    harness.double_click_label("Sound Warm Hall");
    harness.idle(2);
    assert_eq!(chain(&harness, 1), ["Poly Synth", "Reverb"]);
    assert_eq!(harness.project().tracks[1].devices[1].params.get("decay"), Some(&6.0));
    assert_eq!(harness.undo_depth(), undo + 1, "one step");

    // Saving a preset of the instrument alone is still what it was.
    harness.settle();
    harness.click_label("Warm Hall");
    harness.click_label("Save preset\u{2026}");
    harness.idle(2);
    assert!(harness.app.state().naming_preset.is_some() && harness.app.state().preset_form.is_none());
    harness.key(Modifiers::NONE, Key::Escape);
    harness.idle(2);
    assert_eq!(harness.session.lock().saved_presets().len(), 1);
}

#[test]
fn what_was_saved_is_renamed_described_and_deleted_from_its_row_and_deleting_is_asked_about() {
    let mut harness = Harness::new(devices_tab(), |session| {
        let track = session.edit(|project| Ok(project.add_track("Keys"))).unwrap();
        session.add_device(ChainRef::Track(track), SYNTH, Some("warm_pad"), None).unwrap();
        session.add_device(ChainRef::Track(track), REVERB, None, None).unwrap();
        session.save_sound(track, "Warm Hall", "a pad in a hall", &["pad".into()]).unwrap();
    });
    harness.click(harness.header(0, 60.0, 8.0));
    harness.idle(2);
    let synth = harness.project().tracks[0].devices[0].id;
    let reverb = harness.project().tracks[0].devices[1].id;

    // The row's menu opens the form on what the sound says of itself.
    harness.saved_menu("Warm Hall");
    harness.click_label("Rename or describe\u{2026}");
    harness.idle(2);
    let form = harness.app.state().preset_form.clone().expect("the form is up");
    assert_eq!(form.what, FormFor::Edit { uri: SYNTH.into(), name: "Warm Hall".into() });
    assert_eq!(
        (form.name.as_str(), form.description.as_str(), form.tags.as_str()),
        ("Warm Hall", "a pad in a hall", "pad")
    );
    // Another name and other tags; Escape would have left it as it was.
    harness.key(Modifiers::COMMAND, Key::A);
    harness.type_text("Soft Hall");
    harness.form_field("Tags");
    harness.type_text("pad, soft");
    harness.key(Modifiers::NONE, Key::Enter);
    harness.idle(3);
    assert!(harness.app.state().preset_form.is_none());
    let sounds = harness.session.lock().saved_presets().sounds();
    assert_eq!(sounds.len(), 1);
    assert_eq!((sounds[0].info.name.as_str(), sounds[0].info.tags.len()), ("Soft Hall", 2));
    assert_eq!(sounds[0].info.description, "a pad in a hall", "what was not touched stays");
    assert_eq!(sounds[0].after, ["Reverb"]);
    // The track is on it under its new name, its reverb with it.
    let project = harness.project();
    assert_eq!(project.device(synth).unwrap().preset.as_ref().unwrap().name, "Soft Hall");
    assert_eq!(project.device(reverb).unwrap().preset.as_ref().unwrap().bringer(), Some(sounds[0].info.id.as_str()));
    assert!(harness.is_on_screen("Sound Soft Hall") && !harness.is_on_screen("Sound Warm Hall"));

    // Deleting is asked about, and "Cancel" deletes nothing.
    harness.saved_menu("Soft Hall");
    harness.click_label("Delete\u{2026}");
    harness.idle(3);
    assert_eq!(harness.app.state().deleting_preset, Some((SYNTH.to_string(), "Soft Hall".to_string())));
    assert_eq!(harness.session.lock().saved_presets().len(), 1, "nothing is gone before the answer");
    harness.click_label("Cancel");
    harness.idle(2);
    assert!(harness.app.state().deleting_preset.is_none());
    assert_eq!(harness.session.lock().saved_presets().len(), 1);
    // "Delete" does: the sound is gone from the list, and the track keeps
    // the devices it has.
    harness.saved_menu("Soft Hall");
    harness.click_label("Delete\u{2026}");
    harness.idle(3);
    harness.click_label("Delete");
    harness.idle(3);
    assert!(harness.session.lock().saved_presets().is_empty());
    assert!(!harness.is_on_screen("Sound Soft Hall"));
    assert_eq!(chain(&harness, 0), ["Poly Synth", "Reverb"]);

    // A preset of one device is renamed and deleted the same way, from
    // the menu of the device that is on it.
    harness.session.lock().save_preset(synth, "Dark Pad", "", &[]).unwrap();
    harness.idle(3);
    let chip = harness.labeled_beside("Dark Pad", false);
    harness.click(chip);
    harness.click_label("Rename or describe \u{201C}Dark Pad\u{201D}\u{2026}");
    harness.idle(2);
    harness.key(Modifiers::COMMAND, Key::A);
    harness.type_text("Darker Pad");
    harness.key(Modifiers::NONE, Key::Enter);
    harness.idle(3);
    let saved = harness.session.lock().saved_presets().list(SYNTH);
    assert_eq!(saved.iter().map(|preset| preset.name.as_str()).collect::<Vec<_>>(), ["Darker Pad"]);
    let chip = harness.labeled_beside("Darker Pad", false);
    harness.click(chip);
    harness.click_label("Delete \u{201C}Darker Pad\u{201D}\u{2026}");
    harness.idle(3);
    harness.click_label("Delete");
    harness.idle(3);
    assert!(harness.session.lock().saved_presets().is_empty());
}

#[test]
fn a_sound_from_the_browser_becomes_a_tracks_by_a_double_click() {
    let (mut harness, first, ..) = with_two_tracks(devices_tab());
    // The first track is selected, and has an equalizer of its own.
    harness.click(harness.header(0, 60.0, 8.0));
    harness.session.lock().add_device(ChainRef::Track(first), "builtin:eq", None, None).unwrap();
    harness.idle(2);
    let undo = harness.undo_depth();

    harness.toggle_kind("Pad");
    harness.double_click_label("Sound Glass Pad");
    harness.idle(2);
    assert_eq!(chain(&harness, 0), ["Wavetable", "Chorus", "Reverb", "EQ"], "in front of what was there");
    assert_eq!(harness.undo_depth(), undo + 1, "one step for the instrument and its effects");
    assert_eq!(harness.app.state().selected_device, Some(harness.project().tracks[0].devices[0].id));

    // Another of the synth's sounds: its effects in place of the pad's.
    harness.toggle_kind("Pad");
    harness.toggle_kind("Pluck");
    harness.double_click_label("Sound Trance Pluck");
    harness.idle(2);
    assert_eq!(chain(&harness, 0), ["Wavetable", "Delay", "Reverb", "EQ"]);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(chain(&harness, 0), ["Wavetable", "Chorus", "Reverb", "EQ"]);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(chain(&harness, 0), ["EQ"]);
    // A bass is a sound too, and a dry one: it takes the effects away.
    harness.toggle_kind("Pluck");
    harness.toggle_kind("Bass");
    harness.double_click_label("Sound Deep Sub");
    harness.idle(2);
    assert_eq!(chain(&harness, 0), ["Wavetable", "EQ"]);
    harness.toggle_kind("Bass");

    // A search lists whatever matches, of every kind, without the kinds'
    // own rows.
    harness.click(pos2(harness.work_left() / 2.0, top_bar::HEIGHT + browser::HEADER + 8.0 + 11.0));
    harness.type_text("choir");
    harness.idle(2);
    assert!(harness.is_on_screen("Sound Choir") && !harness.is_on_screen("Sound Glass Pad"));
    assert!(!harness.is_on_screen("Sounds of the kind Pad"));
}

#[test]
fn a_sound_dragged_to_a_track_becomes_its_sound_and_below_the_tracks_a_track_of_its_own() {
    let (mut harness, _, second, _) = with_two_tracks(devices_tab());
    let undo = harness.undo_depth();
    harness.toggle_kind("Pad");

    // Over a header: the track is offered, and nothing has changed yet.
    let header = harness.header(1, 60.0, 8.0);
    harness.carry_preset("Glass Pad", header);
    let drag = harness.app.state().preset_drag.clone().expect("the sound is being carried");
    assert_eq!((drag.uri.as_str(), drag.label.as_str()), (WAVETABLE, "Glass Pad"));
    assert_eq!(drag.target, Some(Target::Track(second)));
    assert!(harness.hint().contains("Drop to make Glass Pad the sound of Two"), "{}", harness.hint());
    assert!(chain(&harness, 1).is_empty());
    harness.drag_end(header);
    harness.idle(2);
    assert!(harness.app.state().preset_drag.is_none());
    assert_eq!(chain(&harness, 1), ["Wavetable", "Chorus", "Reverb"]);
    assert_eq!(harness.app.state().selected_track, Some(second));
    assert_eq!(harness.undo_depth(), undo + 1, "one drop, one step");

    // Over a lane it is the lane's track that is offered.
    harness.toggle_kind("Pad");
    harness.toggle_kind("Lead");
    let lane = harness.lane(6.0, 0);
    harness.carry_preset("Anthem Saws", lane);
    let first = harness.project().tracks[0].id;
    assert_eq!(harness.app.state().preset_drag.as_ref().unwrap().target, Some(Target::Track(first)));
    harness.drag_end(lane);
    harness.idle(2);
    assert_eq!(chain(&harness, 0), ["Wavetable", "Delay", "Reverb"]);

    // Below the tracks: a track of its own, called after the sound.
    harness.toggle_kind("Lead");
    harness.toggle_kind("Pad");
    let below = pos2(harness.lane(8.0, 0).x, harness.row_top(2) + 24.0);
    harness.carry_preset("Choir", below);
    assert_eq!(harness.app.state().preset_drag.as_ref().unwrap().target, Some(Target::NewTrack));
    assert!(harness.hint().contains("Drop for a new track with Choir"), "{}", harness.hint());
    let undo = harness.undo_depth();
    harness.drag_end(below);
    harness.idle(2);
    let project = harness.project();
    assert_eq!(project.tracks.len(), 3);
    assert_eq!(project.tracks[2].name, "Choir");
    assert_eq!(chain(&harness, 2), ["Wavetable", "Chorus", "Reverb"]);
    assert_eq!(harness.undo_depth(), undo + 1);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(harness.project().tracks.len(), 2, "one step took the track and its sound back");

    // Escape puts a sound that is in the air down where it was; one let go
    // over nothing that takes it changes nothing either.
    harness.carry_preset("Choir", header);
    harness.key(Modifiers::NONE, Key::Escape);
    assert!(harness.app.state().preset_drag.is_none());
    harness.drag_end(header);
    let nowhere = pos2(harness.work_left() / 2.0, harness.row_top(4));
    harness.carry_preset("Choir", nowhere);
    assert_eq!(harness.app.state().preset_drag.as_ref().unwrap().target, None);
    harness.drag_end(nowhere);
    harness.idle(2);
    assert_eq!(chain(&harness, 1), ["Wavetable", "Chorus", "Reverb"]);
    assert_eq!(harness.project().tracks.len(), 2);
}

#[test]
fn an_instrument_is_not_dropped_on_a_group_and_starts_an_empty_song() {
    // Nothing in the song: the sound makes its first track.
    let mut harness = Harness::new(devices_tab(), |_| {});
    harness.toggle_kind("Pad");
    let middle = pos2(harness.work_left() + 300.0, top_bar::HEIGHT + 200.0);
    harness.carry_preset("Glass Pad", middle);
    assert_eq!(harness.app.state().preset_drag.as_ref().unwrap().target, Some(Target::NewTrack));
    harness.drag_end(middle);
    harness.idle(2);
    assert_eq!(chain(&harness, 0), ["Wavetable", "Chorus", "Reverb"]);

    // A group plays no notes.
    let group = harness
        .session
        .lock()
        .edit(|project| Ok(project.add_track_of_kind("Bus", trill_core::TrackKind::Group)))
        .unwrap();
    harness.idle(2);
    let header = harness.header(1, 60.0, 8.0);
    harness.carry_preset("Choir", header);
    assert_eq!(harness.app.state().preset_drag.as_ref().unwrap().target, None);
    assert!(harness.hint().contains("not on a group or a return"), "{}", harness.hint());
    harness.drag_end(header);
    harness.idle(2);
    assert!(harness.project().track(group).unwrap().devices.is_empty());
}
