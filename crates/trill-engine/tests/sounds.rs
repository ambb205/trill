//! Sounds through the session: an instrument and the effects after it,
//! saved from one song and given to a track of another as one thing.

mod common;

use common::*;
use std::sync::Arc;
use trill_core::device::{
    CompositeFactory, DeviceCategory, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance,
};
use trill_core::presets::{SavedPreset, SoundDevice};
use trill_core::{ChainRef, Device, FilePlace, Note, Relink, TrackId};
use trill_dsp::{BuiltinFactory, Builtins};
use trill_engine::render::RenderOptions;
use trill_engine::scripted::{AMP, BASS, ScriptedFactory};
use trill_engine::{AudioBackend, Problem, Session, listed_sounds};
use trill_media::MediaCache;

const WAVETABLE: &str = "builtin:wavetable";
const SYNTH: &str = "builtin:polysynth";
const REVERB: &str = "builtin:reverb";
const BROKEN: &str = "test:broken";

/// An effect that is installed and does not start.
struct Broken;

impl DeviceFactory for Broken {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        vec![DeviceDescriptor {
            uri: BROKEN.into(),
            name: "Broken Amp".into(),
            vendor: "Trill tests".into(),
            category: DeviceCategory::Effect,
            description: String::new(),
            params: Vec::new(),
            presets: Vec::new(),
        }]
    }

    fn create(&self, device: &Device, _sample_rate: f64, _max_block: usize) -> Result<DeviceInstance, DeviceError> {
        Err(DeviceError::LoadFailed { uri: device.uri.clone(), reason: "it would not open".into() })
    }
}

/// A session with the built-in devices, the scripted plugins and an effect
/// that does not start, without the master limiter.
fn session() -> Session {
    let media = Arc::new(MediaCache::default());
    let factory = CompositeFactory::new(vec![
        Box::new(Builtins::with_loader(media.clone())),
        Box::new(ScriptedFactory::new()),
        Box::new(Broken),
    ]);
    let mut session =
        Session::with_media(Arc::new(factory), media, AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
    let limiter = session.project().master.devices[0].id;
    session.edit(|project| project.remove_device(limiter)).unwrap();
    session
}

fn track(session: &mut Session, name: &str) -> TrackId {
    session.edit(|project| Ok(project.add_track(name))).unwrap()
}

/// The devices of a track, by name.
fn chain(session: &Session, track: TrackId) -> Vec<String> {
    session.project().track(track).unwrap().devices.iter().map(|device| device.name.clone()).collect()
}

/// A phrase of four notes on `track`, and the song rendered.
fn phrase(session: &mut Session, track: TrackId) {
    session
        .edit(|project| {
            let clip = project.add_midi_clip(track, "Phrase", 0.0, 4.0)?;
            let note = |pitch, start| Note { pitch, start, length: 0.75, velocity: 100, ..Default::default() };
            project.clip_mut(clip)?.add_notes(&[note(45, 0.0), note(52, 1.0), note(48, 2.0), note(45, 3.0)])
        })
        .unwrap();
}

fn render(session: &mut Session) -> (Vec<f32>, Vec<f32>) {
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 2.0, ..RenderOptions::default() };
    let out = session.render_job().render(&options).unwrap();
    (out.left, out.right)
}

fn depth(session: &mut Session) -> usize {
    let mut depth = 0;
    while session.undo() {
        depth += 1;
    }
    for _ in 0..depth {
        session.redo();
    }
    depth
}

#[test]
fn a_sound_a_device_brings_makes_its_effects_behind_it_in_one_undo_step() {
    let mut session = session();
    let pads = track(&mut session, "Pads");
    let steps = depth(&mut session);

    let added = session.add_given(ChainRef::Track(pads), WAVETABLE, Some("Glass Pad"), None).unwrap();
    assert_eq!(chain(&session, pads), ["Wavetable", "Chorus", "Reverb"]);
    let given = added.given.unwrap();
    assert!(given.sound && given.removed.is_empty() && given.without.is_empty());
    assert_eq!((given.preset.name.as_str(), given.made.len()), ("Glass Pad", 2));
    // The effects are devices like any other, with what the sound gives
    // them and the rest as they come.
    let reverb = session.project().device(given.made[1]).unwrap();
    assert_eq!((reverb.params.get("decay"), reverb.params.get("mix")), (Some(&4.0), Some(&0.3)));
    assert_eq!(reverb.preset.as_ref().unwrap().bringer(), Some("Glass Pad"));
    assert!(session.device_params(given.made[0]).unwrap().iter().any(|param| param.info.id == "rate"));
    // One step for the instrument and its effects, there and back.
    assert_eq!(depth(&mut session), steps + 1);
    assert!(session.undo());
    assert!(chain(&session, pads).is_empty());
    assert!(session.redo());
    assert_eq!(chain(&session, pads), ["Wavetable", "Chorus", "Reverb"]);

    // A bass is dry and says so; the list of sounds has every sound of
    // the synth's bank, each with what it brings.
    let bass = track(&mut session, "Bass");
    let added = session.add_given(ChainRef::Track(bass), WAVETABLE, Some("Solid Bass"), None).unwrap();
    assert_eq!(chain(&session, bass), ["Wavetable"]);
    assert!(added.given.unwrap().sound);
    let sounds = listed_sounds(&session.descriptors(), session.saved_presets());
    let bank = session.descriptors().iter().find(|device| device.uri == WAVETABLE).unwrap().presets.len();
    assert_eq!(sounds.len(), bank - 1, "all but the plain sound a synth starts from");
    assert!(sounds.iter().any(|sound| sound.preset.name == "Solid Bass" && sound.after.is_empty()));
    assert!(sounds.iter().all(|sound| sound.preset.name != "init"));
    let pad = sounds.iter().find(|sound| sound.preset.name == "Glass Pad").unwrap();
    assert_eq!(
        (pad.device.as_str(), pad.after.as_slice(), pad.saved),
        ("Wavetable", &["Chorus".into(), "Reverb".into()][..], false)
    );
    let info = session.presets(WAVETABLE).presets.iter().find(|preset| preset.name == "Anthem Saws").cloned().unwrap();
    assert_eq!(session.sound_after(WAVETABLE, &info), Some(vec!["Delay".to_string(), "Reverb".to_string()]));
}

#[test]
fn another_sound_replaces_what_the_last_one_brought_and_leaves_the_rest() {
    let mut session = session();
    let keys = track(&mut session, "Keys");
    let on = ChainRef::Track(keys);
    let synth = session.add_device(on, WAVETABLE, Some("Glass Pad"), None).unwrap();
    // An equalizer of the mix, and the pad's reverb given a longer tail.
    session.add_device(on, "builtin:eq", None, None).unwrap();
    let reverb = session.project().track(keys).unwrap().devices[2].id;
    session.set_device_params(reverb, &[("decay".into(), 9.0)]).unwrap();
    assert_eq!(chain(&session, keys), ["Wavetable", "Chorus", "Reverb", "EQ"]);
    let steps = depth(&mut session);

    // A pluck: its echo and its room in place of the pad's chorus and
    // hall, the equalizer behind them; the synth is the device it was.
    let given = session.give_preset(synth, "Trance Pluck").unwrap();
    assert_eq!(chain(&session, keys), ["Wavetable", "Delay", "Reverb", "EQ"]);
    assert_eq!(given.removed, ["Chorus", "Reverb"]);
    assert_eq!(given.made.len(), 2);
    assert_eq!(session.project().track(keys).unwrap().devices[0].id, synth);
    let room = session.project().device(given.made[1]).unwrap();
    assert_eq!(room.params.get("decay"), Some(&2.2), "the pluck's room, not the pad's hall");
    assert_eq!(depth(&mut session), steps + 1, "one step");
    assert!(session.undo());
    assert_eq!(chain(&session, keys), ["Wavetable", "Chorus", "Reverb", "EQ"]);
    assert_eq!(session.project().device(reverb).unwrap().params.get("decay"), Some(&9.0));
    assert!(session.redo());

    // An effect that was given a preset of its own is nobody's sound any
    // more, and stays when the next sound comes; a bass is dry.
    let delay = session.project().track(keys).unwrap().devices[1].id;
    let own = SavedPreset { uri: "builtin:delay".into(), name: "Slap".into(), ..SavedPreset::default() };
    session.saved_presets().save(own).unwrap();
    session.load_preset(delay, "Slap").unwrap();
    let given = session.give_preset(synth, "Solid Bass").unwrap();
    assert_eq!(chain(&session, keys), ["Wavetable", "Delay", "EQ"]);
    assert_eq!((given.sound, given.made.len(), given.removed.as_slice()), (true, 0, &["Reverb".to_string()][..]));

    // A preset of one device says nothing of what follows it: the poly
    // synth's touch nothing behind them.
    let lead = track(&mut session, "Lead");
    let poly = session.add_device(ChainRef::Track(lead), SYNTH, None, None).unwrap();
    session.add_device(ChainRef::Track(lead), REVERB, None, None).unwrap();
    let given = session.give_preset(poly, "warm_pad").unwrap();
    assert!(!given.sound && given.made.is_empty() && given.removed.is_empty());
    assert_eq!(chain(&session, lead), ["Poly Synth", "Reverb"]);
}

#[test]
fn a_tracks_sound_is_saved_and_given_to_a_track_of_another_song_where_it_sounds_the_same() {
    let saved_in = tempfile::tempdir().unwrap();
    let mut first = session();
    first.saved_presets().open(saved_in.path().to_path_buf());
    let kick = track(&mut first, "Kick");
    let bass = track(&mut first, "Bass");
    let on = ChainRef::Track(bass);
    // A plugin instrument on a preset only it can load, through a plugin
    // amplifier, a compressor that listens to the kick and a reverb that
    // was changed and then bypassed.
    let instrument = first.add_device(on, BASS, Some("Octave up"), None).unwrap();
    first.add_device(on, AMP, None, None).unwrap();
    let compressor = first.add_device(on, "builtin:compressor", None, None).unwrap();
    first.set_sidechain(compressor, Some(kick)).unwrap();
    let reverb = first.add_device(on, REVERB, None, None).unwrap();
    first.set_device_params(reverb, &[("decay".into(), 0.8), ("mix".into(), 0.4)]).unwrap();
    first.edit(|project| project.set_track_volume(bass, -7.0)).unwrap();
    phrase(&mut first, bass);
    let steps = depth(&mut first);

    let saved = first.save_sound(bass, " Dub Bass ", "an octave up, through the amp", &["bass".into()]).unwrap();
    assert_eq!(saved.saved.info.path(), "Saved/Dub Bass");
    assert_eq!(saved.uri, BASS);
    assert_eq!(saved.devices, ["Scripted Bass", "Scripted Amp", "Compressor", "Reverb"]);
    // What a sound does not hold is said where the track has any of it.
    assert_eq!(saved.left_out.len(), 1);
    assert!(saved.left_out[0].starts_with("Compressor listens to Kick in this song"), "{:?}", saved.left_out);
    // The track is on that sound from now on, which is a step; and the
    // sound is one of the instrument's presets.
    assert_eq!(depth(&mut first), steps + 1);
    assert_eq!(first.device_preset(instrument).unwrap().name, "Dub Bass");
    assert_eq!(
        first.project().device(reverb).unwrap().preset.as_ref().unwrap().bringer(),
        Some(saved.saved.info.id.as_str())
    );
    assert_eq!(first.project().device(compressor).unwrap().sidechain, Some(kick), "the song keeps its routing");
    assert!(first.presets(BASS).find("Dub Bass") != trill_core::presets::PresetLookup::Missing { similar: Vec::new() });
    assert_eq!(first.sound_after(BASS, &saved.saved.info).map(|after| after.len()), Some(3));

    // The compressor listens to nothing in the sound, so for the two
    // songs to be compared it listens to nothing here either.
    first.set_sidechain(compressor, None).unwrap();
    let there = render(&mut first);
    assert!(peak(&there.0) > 0.01);

    // Another song, in another run of the program: a new track is given
    // the sound by its name, as it would be given a preset.
    let mut second = session();
    second.saved_presets().open(saved_in.path().to_path_buf());
    let part = track(&mut second, "Part");
    let steps = depth(&mut second);
    let added = second.add_given(ChainRef::Track(part), BASS, Some("Dub Bass"), None).unwrap();
    assert_eq!(depth(&mut second), steps + 1, "one step");
    assert_eq!(chain(&second, part), ["Scripted Bass", "Scripted Amp", "Compressor", "Reverb"]);
    let given = added.given.unwrap();
    assert!(given.sound && given.without.is_empty() && given.made.len() == 3);
    let devices = &second.project().track(part).unwrap().devices;
    assert_eq!(devices[2].sidechain, None, "it listens to its own input");
    assert_eq!((devices[3].params.get("decay"), devices[3].params.get("mix")), (Some(&0.8), Some(&0.4)));
    assert_eq!(second.project().track(part).unwrap().volume_db, 0.0, "the fader is the song's, not the sound's");
    // The same notes at the same fader are the same samples.
    second.edit(|project| project.set_track_volume(part, -7.0)).unwrap();
    phrase(&mut second, part);
    let here = render(&mut second);
    assert_eq!(there.0.len(), here.0.len());
    assert!(there == here, "the sound is not what it was in the song it was saved from");

    // A sound with a bypassed effect is given back with it bypassed, and
    // the same name saved again takes the place of the first.
    second
        .edit(|project| {
            project.device_mut(given.made[2])?.enabled = false;
            Ok(())
        })
        .unwrap();
    let again = second.save_sound(part, "dub bass", "", &[]).unwrap();
    assert!(again.saved.replaced && again.left_out.is_empty());
    let other = track(&mut second, "Other");
    second.add_device(ChainRef::Track(other), BASS, Some("dub bass"), None).unwrap();
    assert!(!second.project().track(other).unwrap().devices[3].enabled);
    assert_eq!(
        listed_sounds(&second.descriptors(), second.saved_presets()).iter().filter(|sound| sound.saved).count(),
        1
    );
}

#[test]
fn a_sound_is_made_without_what_this_machine_has_not_got_and_says_which() {
    let mut session = session();
    let after = vec![
        SoundDevice::new("vst3:NOT-HERE", "Big Amp", &[]),
        SoundDevice::new(BROKEN, "Broken Amp", &[]),
        SoundDevice::new(REVERB, "Reverb", &[("decay", 1.2)]),
    ];
    let tone = SavedPreset { uri: SYNTH.into(), name: "Tone".into(), after: Some(after), ..SavedPreset::default() };
    session.saved_presets().save(tone).unwrap();
    let part = track(&mut session, "Part");
    let steps = depth(&mut session);

    let added = session.add_given(ChainRef::Track(part), SYNTH, Some("Tone"), None).unwrap();
    let given = added.given.unwrap();
    // The instrument and the reverb are made; the two amplifiers are said.
    assert_eq!(chain(&session, part), ["Poly Synth", "Reverb"]);
    assert_eq!(given.made.len(), 1);
    assert_eq!(
        given.without,
        [
            "Big Amp (vst3:NOT-HERE) is not installed on this machine",
            "Broken Amp (test:broken) could not be started: failed to load 'test:broken': it would not open",
        ]
    );
    assert!(session.problems().is_empty(), "nothing of it is left in the song: {:?}", session.problems());
    assert_eq!(depth(&mut session), steps + 1, "still one step");
    assert!(session.undo());
    assert!(chain(&session, part).is_empty());
    assert!(session.redo());

    // Given to the device that is there, it says the same.
    let synth = session.project().track(part).unwrap().devices[0].id;
    let steps = depth(&mut session);
    let given = session.give_preset(synth, "Tone").unwrap();
    assert_eq!((given.without.len(), given.removed.as_slice()), (2, &["Reverb".to_string()][..]));
    assert_eq!(chain(&session, part), ["Poly Synth", "Reverb"]);
    assert_eq!(depth(&mut session), steps + 1);

    // A sound of an instrument this machine has not got is nobody's to
    // give, and is not listed as one.
    let lost =
        SavedPreset { uri: "vst3:GONE".into(), name: "Lost".into(), after: Some(Vec::new()), ..SavedPreset::default() };
    session.saved_presets().save(lost).unwrap();
    assert!(
        listed_sounds(&session.descriptors(), session.saved_presets()).iter().all(|sound| sound.preset.name != "Lost")
    );
    let error = session.add_device(ChainRef::Track(part), "vst3:GONE", Some("Lost"), None).unwrap_err();
    assert!(error.to_string().contains("unknown device 'vst3:GONE'"), "{error}");
}

#[test]
fn a_track_with_no_sound_to_save_says_so_and_a_track_without_an_instrument_saves_its_effects() {
    let mut session = session();
    let empty = track(&mut session, "Empty");
    let error = session.save_sound(empty, "Nothing", "", &[]).unwrap_err();
    assert!(error.to_string().contains("has no devices, so there is no sound to save"), "{error}");
    assert!(session.save_sound(empty, "a/b", "", &[]).unwrap_err().to_string().contains("cannot have '/'"));

    // An audio track's chain: an amplifier and a room, a sound of the
    // amplifier.
    let guitar = track(&mut session, "Guitar DI");
    session.add_device(ChainRef::Track(guitar), AMP, None, None).unwrap();
    session.add_device(ChainRef::Track(guitar), REVERB, None, None).unwrap();
    let saved = session.save_sound(guitar, "Room Amp", "", &[]).unwrap();
    assert_eq!((saved.uri.as_str(), saved.devices.len()), (AMP, 2));
    let other = track(&mut session, "Other DI");
    session.add_device(ChainRef::Track(other), AMP, Some("Room Amp"), None).unwrap();
    assert_eq!(chain(&session, other), ["Scripted Amp", "Reverb"]);

    // An effect in front of the instrument is not part of its sound.
    let keys = track(&mut session, "Keys");
    session.add_device(ChainRef::Track(keys), "builtin:filter", None, None).unwrap();
    session.add_device(ChainRef::Track(keys), SYNTH, None, None).unwrap();
    let saved = session.save_sound(keys, "Plain", "", &[]).unwrap();
    assert_eq!(saved.devices, ["Poly Synth"]);
    assert_eq!(saved.left_out, ["Auto Filter comes before the instrument and is not part of the sound"]);
}

#[test]
fn the_files_a_sounds_devices_name_are_listed_and_relinked_like_any_others() {
    let folder = folder();
    let saved_in = folder.path().join("saved");
    let kick = write_wav(&folder.path().join("kick.wav"), 44_100.0, &sine(110.0, 44_100.0, 0.5, 0.5));
    let mut first = session();
    first.saved_presets().open(saved_in.clone());
    let kit = track(&mut first, "Kit");
    let sampler = first.add_device(ChainRef::Track(kit), "builtin:sampler", None, None).unwrap();
    first.set_sampler_file(sampler, Some(&kick)).unwrap();
    first.add_device(ChainRef::Track(kit), REVERB, None, None).unwrap();
    first.save_sound(kit, "Boom", "", &[]).unwrap();

    // The file goes elsewhere; the sound is given to a track of another
    // song all the same, and the song says what it is missing.
    let moved = folder.path().join("moved");
    std::fs::create_dir_all(&moved).unwrap();
    let now = moved.join("kick.wav");
    std::fs::rename(&kick, &now).unwrap();
    let mut second = session();
    second.saved_presets().open(saved_in);
    let part = track(&mut second, "Part");
    let made = second.add_device(ChainRef::Track(part), "builtin:sampler", Some("Boom"), None).unwrap();
    assert_eq!(chain(&second, part), ["Sampler", "Reverb"]);
    let files = second.files();
    assert_eq!(files.len(), 1);
    assert_eq!(
        (files[0].path.as_str(), &files[0].place),
        (kick.as_str(), &FilePlace::Sample { device: made, slot: "sample".into() })
    );
    let problems: Vec<String> = second.problems().iter().map(Problem::to_string).collect();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("kick.wav"), "{problems:?}");

    // Given the file where it is now, the song is whole.
    let relink = Relink { place: files[0].place.clone(), path: now.to_string_lossy().into_owned() };
    assert_eq!(second.relink(&[relink]).unwrap(), 1);
    assert!(second.problems().is_empty(), "{:?}", second.problems());
    assert_eq!(second.files()[0].path, now.to_string_lossy());
}

#[test]
fn a_sound_dropped_on_a_track_goes_where_its_instrument_has_its_place() {
    let mut session = session();
    // A track without an instrument: the sound goes in front of what is
    // there.
    let part = track(&mut session, "Part");
    session.add_device(ChainRef::Track(part), "builtin:eq", None, None).unwrap();
    let steps = depth(&mut session);
    let added = session.give_track(part, WAVETABLE, "Glass Pad").unwrap();
    assert_eq!(chain(&session, part), ["Wavetable", "Chorus", "Reverb", "EQ"]);
    assert_eq!(depth(&mut session), steps + 1);
    let synth = added.device;

    // The same instrument takes another of its sounds on as it is.
    let again = session.give_track(part, WAVETABLE, "Anthem Saws").unwrap();
    assert_eq!(again.device, synth);
    assert_eq!(chain(&session, part), ["Wavetable", "Delay", "Reverb", "EQ"]);
    assert_eq!(again.given.unwrap().removed, ["Chorus", "Reverb"]);

    // A sound of another instrument: that instrument in the place of the
    // one that was there, its effects in the place of the last sound's,
    // in one step that takes all of it back.
    let tone = SavedPreset {
        uri: BASS.into(),
        name: "Tone".into(),
        after: Some(vec![SoundDevice::new(AMP, "Scripted Amp", &[])]),
        ..SavedPreset::default()
    };
    session.saved_presets().save(tone).unwrap();
    let steps = depth(&mut session);
    let other = session.give_track(part, BASS, "Tone").unwrap();
    assert_eq!(chain(&session, part), ["Scripted Bass", "Scripted Amp", "EQ"]);
    assert_eq!(other.given.unwrap().removed, ["Delay", "Reverb"]);
    assert_eq!(depth(&mut session), steps + 1);
    assert!(session.undo());
    assert_eq!(chain(&session, part), ["Wavetable", "Delay", "Reverb", "EQ"]);
    assert_eq!(session.project().track(part).unwrap().devices[0].id, synth);
    assert!(session.redo());

    // What cannot be given leaves the track as it was: a preset there is
    // none of, and an instrument there is none of.
    let before = session.project().clone();
    let steps = depth(&mut session);
    assert!(session.give_track(part, WAVETABLE, "No Such Sound").unwrap_err().to_string().contains("has no preset"));
    assert!(session.give_track(part, "vst3:GONE", "Tone").unwrap_err().to_string().contains("unknown device"));
    assert_eq!(depth(&mut session), steps);
    assert_eq!(session.project(), &before);

    // A sound that begins with an effect is taken on by a device of that
    // type, and otherwise goes to the end of the chain.
    let room = SavedPreset {
        uri: AMP.into(),
        name: "Room Amp".into(),
        after: Some(vec![SoundDevice::new(REVERB, "Reverb", &[])]),
        ..SavedPreset::default()
    };
    session.saved_presets().save(room).unwrap();
    session.give_track(part, AMP, "Room Amp").unwrap();
    assert_eq!(chain(&session, part), ["Scripted Bass", "Scripted Amp", "Reverb", "EQ"]);
    let di = track(&mut session, "DI");
    session.add_device(ChainRef::Track(di), "builtin:eq", None, None).unwrap();
    session.give_track(di, AMP, "Room Amp").unwrap();
    assert_eq!(chain(&session, di), ["EQ", "Scripted Amp", "Reverb"]);
}

#[test]
fn a_sound_given_with_builtin_devices_is_what_the_patch_says() {
    // What ships is what the table of patches says, to the device: a
    // track given a patch through the session has the chain a project
    // gets from the patch by hand.
    let mut session = Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
    let descriptors = session.descriptors();
    let wavetable = descriptors.iter().find(|descriptor| descriptor.uri == WAVETABLE).unwrap();
    for preset in &wavetable.presets {
        let part = track(&mut session, &preset.name);
        session.add_device(ChainRef::Track(part), WAVETABLE, Some(&preset.name), None).unwrap();
        let made: Vec<(String, Vec<(String, f64)>)> = session.project().track(part).unwrap().devices[1..]
            .iter()
            .map(|device| (device.uri.clone(), device.params.clone().into_iter().collect()))
            .collect();
        let said: Vec<(String, Vec<(String, f64)>)> = preset
            .after
            .iter()
            .flatten()
            .map(|device| (device.uri.clone(), device.params.clone().into_iter().collect()))
            .collect();
        assert_eq!(made, said, "{}", preset.name);
    }
}
