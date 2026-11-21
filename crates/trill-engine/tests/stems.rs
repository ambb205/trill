//! One render in place of many: the stems a render writes as it goes are
//! what separate renders of each part of the song give, and a render that
//! hears only some tracks makes only those.
//!
//! The song here has a built-in synth with an effect and a send, a bass
//! and drums (scripted instruments, which the engine treats as plugins)
//! in a group with a compressor on it, a return with a reverb, a limiter
//! on the master, a muted track and a track without clips.

use std::path::Path;
use std::sync::Arc;
use trill_core::device::{CompositeFactory, DeviceFactory};
use trill_core::project::{AutomationPoint, AutomationTarget, Segment};
use trill_core::{ChainRef, DeviceId, Note, Project, TrackId, TrackKind};
use trill_dsp::BuiltinFactory;
use trill_engine::render::{RenderJob, RenderOptions, Rendered, StemKind, Stems};
use trill_engine::scripted::{AMP, BASS, DRUMS, Loading, ScriptedFactory};
use trill_engine::sources::AudioSources;

const SR: u32 = 48_000;

struct Song {
    project: Project,
    synth: TrackId,
    bass: TrackId,
    drums: TrackId,
    band: TrackId,
    room: TrackId,
    muted: TrackId,
    empty: TrackId,
}

fn note(pitch: u8, start: f64, length: f64) -> Note {
    Note { pitch, start, length, velocity: 100, ..Default::default() }
}

fn device(project: &mut Project, chain: ChainRef, uri: &str, params: &[(&str, f64)]) -> DeviceId {
    let id = project.add_device(chain, uri, uri, None).unwrap();
    let device = project.device_mut(id).unwrap();
    device.params.extend(params.iter().map(|(name, value)| ((*name).to_string(), *value)));
    id
}

fn part(project: &mut Project, name: &str, instrument: &str, notes: &[Note]) -> TrackId {
    let track = project.add_track(name);
    device(project, ChainRef::Track(track), instrument, &[]);
    let clip = project.add_midi_clip(track, "clip", 0.0, 8.0).unwrap();
    project.clip_mut(clip).unwrap().add_notes(notes).unwrap();
    track
}

fn song() -> Song {
    let mut project = Project::new("stems");
    let room = project.add_track_of_kind("Room", TrackKind::Return);
    device(&mut project, ChainRef::Track(room), "builtin:reverb", &[("mix", 1.0)]);
    let band = project.add_track_of_kind("Band", TrackKind::Group);
    device(&mut project, ChainRef::Track(band), "builtin:compressor", &[("threshold", -30.0), ("makeup", 3.0)]);
    project.track_mut(band).unwrap().volume_db = -2.0;

    let synth = part(&mut project, "Synth: lead/1", "builtin:polysynth", &[note(60, 0.0, 1.0), note(67, 2.0, 1.5)]);
    device(&mut project, ChainRef::Track(synth), "builtin:delay", &[]);
    project.track_mut(synth).unwrap().volume_db = -3.0;
    project.track_mut(synth).unwrap().pan = 0.3;
    project.set_send(synth, room, Some(-6.0)).unwrap();

    let bass = part(&mut project, "Bass", BASS, &[note(40, 0.0, 1.0), note(43, 1.0, 1.0), note(45, 3.0, 1.0)]);
    device(&mut project, ChainRef::Track(bass), AMP, &[]);
    project.set_track_group(bass, Some(band)).unwrap();
    project.set_send(bass, room, Some(-10.0)).unwrap();
    project.track_mut(bass).unwrap().pan = -0.2;

    let drums = part(&mut project, "Drums", DRUMS, &[note(36, 0.0, 0.25), note(38, 1.0, 0.25), note(49, 2.0, 0.25)]);
    project.set_track_group(drums, Some(band)).unwrap();

    let muted = part(&mut project, "Muted", BASS, &[note(47, 0.0, 2.0)]);
    project.track_mut(muted).unwrap().mute = true;
    let empty = project.add_track("Empty");
    device(&mut project, ChainRef::Master, "builtin:limiter", &[("gain", 6.0)]);
    project.master.volume_db = -1.0;
    Song { project, synth, bass, drums, band, room, muted, empty }
}

fn factory(scripted: &ScriptedFactory) -> Arc<dyn DeviceFactory> {
    Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(scripted.clone())]))
}

fn options() -> RenderOptions {
    RenderOptions { sample_rate: SR, max_tail_seconds: 4.0, ..RenderOptions::default() }
}

fn render(project: &Project, scripted: &ScriptedFactory, options: &RenderOptions) -> Rendered {
    let job =
        RenderJob { project: project.clone(), factory: factory(scripted), audio: AudioSources::default(), apart: None };
    job.render(options).unwrap()
}

/// `project` with nothing after the tracks: no master devices, the master
/// at 0 dB. What it renders to is the sum of its tracks.
fn bare_master(project: &Project) -> Project {
    let mut bare = project.clone();
    bare.master.devices.clear();
    bare.master.volume_db = 0.0;
    bare
}

/// `project` with only `kept` left in it.
fn only(project: &Project, kept: &[TrackId]) -> Project {
    let mut only = bare_master(project);
    let others: Vec<TrackId> = only.tracks.iter().map(|track| track.id).filter(|id| !kept.contains(id)).collect();
    for other in others {
        only.remove_track(other).unwrap();
    }
    only
}

fn read(path: &str) -> (Vec<f32>, Vec<f32>) {
    let audio = trill_media::decode(Path::new(path)).unwrap_or_else(|error| panic!("{path}: {error}"));
    assert_eq!(audio.sample_rate, f64::from(SR), "{path}");
    (audio.left, audio.right)
}

/// How far two pieces of audio are apart at most, over the length they
/// share, and whether what one has more of than the other is silence.
fn apart(what: &str, (left, right): &(Vec<f32>, Vec<f32>), (other_left, other_right): (&[f32], &[f32])) -> f32 {
    let shared = left.len().min(other_left.len());
    assert!(shared > SR as usize, "{what}: there is something to compare");
    let rest = |audio: &[f32]| audio[shared..].iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    let beyond = rest(left).max(rest(right)).max(rest(other_left)).max(rest(other_right));
    assert!(beyond < 1e-4, "{what}: what one has beyond the other is silence, not {beyond}");
    let difference = |a: &[f32], b: &[f32]| {
        a[..shared].iter().zip(&b[..shared]).fold(0.0f32, |most, (a, b)| most.max((a - b).abs()))
    };
    difference(left, other_left).max(difference(right, other_right))
}

fn peak(audio: &[f32]) -> f32 {
    audio.iter().fold(0.0, |peak, sample| peak.max(sample.abs()))
}

#[test]
fn the_stems_of_one_render_are_what_separate_renders_of_each_part_give() {
    let Song { project, synth, bass, drums, band, room, muted, empty } = song();
    let scripted = ScriptedFactory::new();
    let folder = tempfile::tempdir().unwrap();
    let stems = Stems::all(folder.path().join("made here"));
    let whole = render(&project, &scripted, &RenderOptions { stems: Some(stems), ..options() });
    let made = scripted.made().len();

    // What was written, in the order of the song's tracks, each named for
    // what it is; nothing for the muted track or the one without clips.
    let listed: Vec<(StemKind, Option<TrackId>, String)> = whole
        .measured
        .stems
        .iter()
        .map(|stem| (stem.what, stem.track, Path::new(&stem.path).file_name().unwrap().to_string_lossy().into_owned()))
        .collect();
    let file = |what: &str, track: TrackId, name: &str| format!("{what} {} {name}.wav", track.0);
    assert_eq!(
        listed,
        [
            (StemKind::Return, Some(room), file("return", room, "Room")),
            (StemKind::Group, Some(band), file("group", band, "Band")),
            (StemKind::Track, Some(synth), file("track", synth, "Synth_ lead_1")),
            (StemKind::Instrument, Some(synth), file("instrument", synth, "Synth_ lead_1")),
            (StemKind::Track, Some(bass), file("track", bass, "Bass")),
            (StemKind::Instrument, Some(bass), file("instrument", bass, "Bass")),
            (StemKind::Track, Some(drums), file("track", drums, "Drums")),
            (StemKind::Instrument, Some(drums), file("instrument", drums, "Drums")),
            (StemKind::Premaster, None, "premaster.wav".to_string()),
        ]
    );
    assert!(whole.measured.stems.iter().all(|stem| stem.track != Some(muted) && stem.track != Some(empty)));
    let on_disk = std::fs::read_dir(folder.path().join("made here")).unwrap().count();
    assert_eq!(on_disk, listed.len(), "and nothing else is left in the folder");
    let stem = |what: StemKind, track: Option<TrackId>| {
        let stem = whole.measured.stems.iter().find(|stem| stem.what == what && stem.track == track).unwrap();
        (stem, read(&stem.path))
    };

    // Every file is as long as the mix, and is measured as the mix is.
    for listed in &whole.measured.stems {
        let (left, right) = read(&listed.path);
        assert_eq!((left.len(), right.len()), (whole.left.len(), whole.left.len()), "{}", listed.path);
        let heard = 20.0 * peak(&left).max(peak(&right)).log10();
        assert!(
            (listed.stats.peak_db - heard).abs() < 0.01,
            "{}: {} against {heard}",
            listed.path,
            listed.stats.peak_db
        );
        assert!(listed.stats.true_peak_db.is_some() && listed.stats.loudness_lufs > -60.0, "{listed:?}");
    }

    // A track as it leaves its fader is a project that holds that track
    // alone, rendered: after its effects, its fader and its pan, before
    // its group, without what its sends give the returns.
    for track in [synth, bass, drums] {
        let alone = render(&only(&project, &[track]), &scripted, &options());
        let (listed, audio) = stem(StemKind::Track, Some(track));
        let off = apart(&listed.path, &audio, (&alone.left, &alone.right));
        assert!(off < 1e-6, "{}: {off} from the track rendered alone", listed.path);
        let level = whole.measured.tracks.iter().find(|level| level.track == track).unwrap();
        assert!((level.peak_db - listed.stats.peak_db).abs() < 0.01, "the track's own row says the same");
    }

    // What goes into a track's effects is that track with nothing after
    // its instrument, its fader at 0 dB and its pan in the middle.
    for track in [synth, bass, drums] {
        let mut bare = only(&project, &[track]);
        let kept = bare.track_mut(track).unwrap();
        kept.devices.truncate(1);
        (kept.volume_db, kept.pan) = (0.0, 0.0);
        let alone = render(&bare, &scripted, &options());
        let (listed, audio) = stem(StemKind::Instrument, Some(track));
        let off = apart(&listed.path, &audio, (&alone.left, &alone.right));
        assert!(off < 1e-6, "{}: {off} from the instrument rendered alone", listed.path);
    }
    // (Which is not what leaves the fader, where the track has effects.)
    let (_, source) = stem(StemKind::Instrument, Some(bass));
    let (_, faded) = stem(StemKind::Track, Some(bass));
    assert!(apart("the bass before and after its amplifier", &source, (&faded.0, &faded.1)) > 0.05);

    // The group: its members through its own devices and fader.
    let members = render(&only(&project, &[band, bass, drums]), &scripted, &options());
    let (listed, group) = stem(StemKind::Group, Some(band));
    let off = apart(&listed.path, &group, (&members.left, &members.right));
    assert!(off < 1e-6, "{}: {off} from the group rendered alone", listed.path);

    // The return, fed by every track's send, with no track heard itself:
    // the song with everything else behind a muted group. (A track's sends
    // are taken before its group.)
    let mut sends = bare_master(&project);
    let silenced = sends.add_track_of_kind("Silenced", TrackKind::Group);
    sends.set_track_group(synth, Some(silenced)).unwrap();
    for group in [silenced, band] {
        sends.track_mut(group).unwrap().mute = true;
    }
    let returned = render(&sends, &scripted, &options());
    let (listed, returns) = stem(StemKind::Return, Some(room));
    let off = apart(&listed.path, &returns, (&returned.left, &returned.right));
    assert!(off < 1e-6, "{}: {off} from the return rendered alone", listed.path);
    assert!(peak(&returns.0) > 0.01, "and it is not silence");
    // Which is what hearing the return by itself gives, and soloing it.
    let named = RenderOptions { only_tracks: Some(vec![room]), ..options() };
    let alone = render(&bare_master(&project), &scripted, &named);
    let off = apart("the return heard alone", &returns, (&alone.left, &alone.right));
    assert!(off < 1e-6, "{off} from the return's stem");
    let mut soloed = bare_master(&project);
    soloed.track_mut(room).unwrap().solo = true;
    let alone = render(&soloed, &scripted, &options());
    assert!(apart("the return soloed", &returns, (&alone.left, &alone.right)) < 1e-6);
    // The group heard by itself, in a song without the return, is its stem.
    let named = RenderOptions { only_tracks: Some(vec![band]), ..options() };
    let mut dry = bare_master(&project);
    dry.remove_track(room).unwrap();
    let alone = render(&dry, &scripted, &named);
    assert!(apart("the group heard alone", &group, (&alone.left, &alone.right)) < 1e-6);

    // The mix as it reaches the master's devices: the song without them.
    let before = render(&bare_master(&project), &scripted, &options());
    let (listed, premaster) = stem(StemKind::Premaster, None);
    let off = apart(&listed.path, &premaster, (&before.left, &before.right));
    assert!(off < 1e-6, "{}: {off} from the song without its master", listed.path);
    // And it is what the tracks outside a group, the groups and the
    // returns add up to.
    let (_, lead) = stem(StemKind::Track, Some(synth));
    let sum = |parts: [&Vec<f32>; 3]| -> Vec<f32> {
        (0..parts[0].len()).map(|frame| parts.iter().map(|part| part[frame]).sum()).collect()
    };
    let (left, right) = (sum([&lead.0, &group.0, &returns.0]), sum([&lead.1, &group.1, &returns.1]));
    let off = apart("the sum of the stems", &premaster, (&left, &right));
    assert!(off < 1e-5, "{off}");

    // Writing stems changes nothing about the mix, and costs no devices.
    let scripted_again = ScriptedFactory::new();
    let plain = render(&project, &scripted_again, &options());
    assert!(plain.left == whole.left && plain.right == whole.right);
    assert!(plain.measured.stems.is_empty());
    assert_eq!(scripted_again.made().len(), made);
    // The muted track's instrument was never made: a bass, its amplifier
    // and the drums were.
    assert_eq!(made, 3, "{:?}", scripted.made());
}

#[test]
fn only_the_stems_that_are_asked_for_are_written_and_a_folder_that_cannot_be_made_is_a_sentence() {
    let Song { project, room, .. } = song();
    let scripted = ScriptedFactory::new();
    let folder = tempfile::tempdir().unwrap();
    let stems = Stems { folder: folder.path().to_path_buf(), returns: true, premaster: true, ..Stems::default() };
    let rendered = render(&project, &scripted, &RenderOptions { stems: Some(stems), ..options() });
    let listed: Vec<(StemKind, Option<TrackId>)> =
        rendered.measured.stems.iter().map(|stem| (stem.what, stem.track)).collect();
    assert_eq!(listed, [(StemKind::Return, Some(room)), (StemKind::Premaster, None)]);
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 2);

    // A file where the folder should be: said before anything is waited
    // for, and nothing is rendered.
    let taken = folder.path().join("taken");
    std::fs::write(&taken, b"a file").unwrap();
    let slow = ScriptedFactory::loading(Loading { first: std::time::Duration::from_secs(30), ..Loading::default() });
    let job =
        RenderJob { project: project.clone(), factory: factory(&slow), audio: AudioSources::default(), apart: None };
    let begun = std::time::Instant::now();
    let refused = job.render(&RenderOptions { stems: Some(Stems::all(&taken)), ..options() });
    let error = refused.err().expect("the folder cannot be made").to_string();
    assert!(error.contains("taken"), "{error}");
    assert!(begun.elapsed() < std::time::Duration::from_secs(10), "{:?}", begun.elapsed());
}

#[test]
fn a_render_that_hears_one_track_makes_and_waits_for_only_what_that_track_needs() {
    let Song { project, synth, bass, drums, band, room, .. } = song();
    // Every scripted instrument takes a second and a half to load.
    let loading = Loading { first: std::time::Duration::from_millis(1500), ..Loading::default() };

    // The synth alone: nothing that has to load is made, and nothing is
    // waited for. What comes out is the song without the other tracks:
    // the synth, the return it sends to, and the master.
    let scripted = ScriptedFactory::loading(loading);
    let heard = render(&project, &scripted, &RenderOptions { only_tracks: Some(vec![synth]), ..options() });
    assert!(scripted.made().is_empty(), "{:?}", scripted.made());
    assert!(heard.measured.waited.is_empty(), "{:?}", heard.measured.waited);
    let mut without = project.clone();
    for other in [bass, drums, band] {
        without.remove_track(other).unwrap();
    }
    let expected = render(&without, &scripted, &options());
    assert!(heard.left == expected.left && heard.right == expected.right);
    assert!(peak(&heard.left) > 0.05);
    // Only what was played has a row.
    let rows: Vec<TrackId> = heard.measured.tracks.iter().map(|level| level.track).collect();
    assert_eq!(rows, [room, synth]);

    // The bass alone: itself and its amplifier, through its group and its
    // return; the drums beside it in the group are not made.
    let scripted = ScriptedFactory::loading(loading);
    let heard = render(&project, &scripted, &RenderOptions { only_tracks: Some(vec![bass]), ..options() });
    let made: Vec<String> = scripted.made().into_iter().map(|(uri, _)| uri).collect();
    assert_eq!(made, [BASS, AMP]);
    let mut without = project.clone();
    for other in [synth, drums] {
        without.remove_track(other).unwrap();
    }
    let expected = render(&without, &ScriptedFactory::new(), &options());
    assert!(heard.left == expected.left && heard.right == expected.right);

    // The return: the tracks whose sends reach it are made, to feed it,
    // and are not heard themselves; the drums, which send nothing, are not
    // made at all.
    let scripted = ScriptedFactory::loading(loading);
    let heard = render(&project, &scripted, &RenderOptions { only_tracks: Some(vec![room]), ..options() });
    let made: Vec<String> = scripted.made().into_iter().map(|(uri, _)| uri).collect();
    assert_eq!(made, [BASS, AMP]);
    assert!(peak(&heard.left) > 0.01, "a return heard alone is not silence");
    let rows: Vec<TrackId> = heard.measured.tracks.iter().map(|level| level.track).collect();
    assert_eq!(rows, [room], "what feeds it has no row of its own");

    // The group: both of its members, and not the synth.
    let scripted = ScriptedFactory::loading(loading);
    let stems = tempfile::tempdir().unwrap();
    let all = Stems::all(stems.path());
    let options = RenderOptions { only_tracks: Some(vec![band]), stems: Some(all), ..options() };
    let heard = render(&project, &scripted, &options);
    let mut made: Vec<String> = scripted.made().into_iter().map(|(uri, _)| uri).collect();
    made.sort();
    assert_eq!(made, [AMP, BASS, DRUMS]);
    // And only what is played has a file.
    let files: Vec<Option<TrackId>> = heard.measured.stems.iter().map(|stem| stem.track).collect();
    assert_eq!(files, [Some(room), Some(band), Some(bass), Some(bass), Some(drums), Some(drums), None]);
}

#[test]
fn a_soloed_return_is_heard_as_every_track_feeds_it_and_a_soloed_track_with_its_returns() {
    // Keys send to both returns. The bass is in a group that sends to the
    // hall, and sends nothing itself. A muted pad sends to the hall too.
    let mut project = Project::new("solo");
    let hall = project.add_track_of_kind("Hall", TrackKind::Return);
    device(&mut project, ChainRef::Track(hall), "builtin:reverb", &[("mix", 1.0)]);
    let echo = project.add_track_of_kind("Echo", TrackKind::Return);
    device(&mut project, ChainRef::Track(echo), "builtin:delay", &[("mix", 1.0)]);
    let low = project.add_track_of_kind("Low", TrackKind::Group);
    project.set_send(low, hall, Some(-3.0)).unwrap();
    let keys = part(&mut project, "Keys", "builtin:polysynth", &[note(60, 0.0, 2.0), note(64, 2.0, 2.0)]);
    project.set_send(keys, hall, Some(-6.0)).unwrap();
    project.set_send(keys, echo, Some(-9.0)).unwrap();
    let bass = part(&mut project, "Bass", BASS, &[note(40, 0.0, 1.0), note(43, 1.0, 3.0)]);
    project.set_track_group(bass, Some(low)).unwrap();
    let pad = part(&mut project, "Pad", "builtin:polysynth", &[note(48, 0.0, 4.0)]);
    project.set_send(pad, hall, Some(0.0)).unwrap();
    project.track_mut(pad).unwrap().mute = true;

    // The song as it is, with every part of it in a file.
    let scripted = ScriptedFactory::new();
    let folder = tempfile::tempdir().unwrap();
    let all = RenderOptions { stems: Some(Stems::all(folder.path())), ..options() };
    let whole = render(&project, &scripted, &all);
    let stem = |what: StemKind, track: TrackId| {
        let stem = whole.measured.stems.iter().find(|stem| stem.what == what && stem.track == Some(track)).unwrap();
        read(&stem.path)
    };
    let (in_hall, in_echo, of_keys) =
        (stem(StemKind::Return, hall), stem(StemKind::Return, echo), stem(StemKind::Track, keys));
    assert!(peak(&in_hall.0) > 0.01 && peak(&in_echo.0) > 0.005 && peak(&of_keys.0) > 0.05);

    // The hall soloed is the hall as the whole song feeds it: the keys'
    // send, and the group's, which carries the bass. No track is heard
    // itself, and the echo is not heard.
    let mut soloed = project.clone();
    soloed.track_mut(hall).unwrap().solo = true;
    let heard = render(&soloed, &scripted, &options());
    let off = apart("the hall soloed", &in_hall, (&heard.left, &heard.right));
    assert!(off < 1e-6, "{off} from the hall's part of the whole song");

    // The keys soloed beside it: the keys, the hall as before, and the
    // echo, which stays open for a soloed track and has the keys in it.
    soloed.track_mut(keys).unwrap().solo = true;
    let heard = render(&soloed, &scripted, &options());
    let sum = |parts: [&Vec<f32>; 3]| -> Vec<f32> {
        (0..parts[0].len()).map(|frame| parts.iter().map(|part| part[frame]).sum()).collect()
    };
    let expected = (sum([&of_keys.0, &in_hall.0, &in_echo.0]), sum([&of_keys.1, &in_hall.1, &in_echo.1]));
    let off = apart("the keys and the hall soloed", &expected, (&heard.left, &heard.right));
    assert!(off < 1e-5, "{off} from the keys, the hall and the echo added up");

    // The keys soloed alone are heard with both returns, as ever, and the
    // returns then hold the keys only: less than the hall of the song.
    soloed.track_mut(hall).unwrap().solo = false;
    let heard = render(&soloed, &scripted, &options());
    let off = apart("the keys soloed", &expected, (&heard.left, &heard.right));
    assert!(off > 1e-3, "the bass is no longer in the hall: {off}");
    let mut without = project.clone();
    for other in [bass, low, pad] {
        without.remove_track(other).unwrap();
    }
    let expected = render(&without, &scripted, &options());
    assert!(apart("the keys soloed", &(heard.left, heard.right), (&expected.left, &expected.right)) < 1e-6);

    // A muted return that is soloed is silence, and a return that nothing
    // sends to is not made for a soloed track.
    let mut muted = project.clone();
    for change in [|track: &mut trill_core::Track| track.solo = true, |track: &mut trill_core::Track| track.mute = true]
    {
        change(muted.track_mut(hall).unwrap());
    }
    let silent = RenderJob { project: muted, factory: factory(&scripted), audio: AudioSources::default(), apart: None };
    let silent = silent.render(&RenderOptions { max_tail_seconds: 0.5, ..options() }).unwrap();
    assert_eq!(peak(&silent.left), 0.0);
}

#[test]
fn a_return_fed_by_sends_that_move_is_heard_alone_as_it_is_in_the_whole_song() {
    // The keys' send to the echo is its envelope alone: thrown open for
    // one note, and no send set by hand. The bass sends by hand, and an
    // envelope moves that send. The pad's send is an envelope someone
    // switched off: it sends nothing. The drums send nothing at all.
    let mut project = Project::new("throws");
    let echo = project.add_track_of_kind("Echo", TrackKind::Return);
    device(&mut project, ChainRef::Track(echo), "builtin:delay", &[("mix", 1.0)]);
    project.track_mut(echo).unwrap().volume_db = -2.0;
    let keys = part(&mut project, "Keys", "builtin:polysynth", &[note(60, 0.0, 2.0), note(64, 2.0, 2.0)]);
    let bass = part(&mut project, "Bass", BASS, &[note(40, 0.0, 1.0), note(43, 1.0, 3.0)]);
    let pad = part(&mut project, "Pad", "builtin:polysynth", &[note(48, 0.0, 4.0)]);
    let drums = part(&mut project, "Drums", DRUMS, &[note(36, 0.0, 0.25), note(38, 1.0, 0.25)]);
    let point = AutomationPoint::new;
    let held = |beat: f64, value: f64| AutomationPoint { beat, value, shape: Segment::Hold };
    let send = |track: TrackId| AutomationTarget::Send { track, to: echo };
    let throw = vec![point(0.0, -70.0), point(1.75, -70.0), point(2.0, -3.0), held(3.0, -3.0), point(3.0, -70.0)];
    project.set_automation(send(keys), throw).unwrap();
    project.set_send(bass, echo, Some(-12.0)).unwrap();
    project.set_automation(send(bass), vec![point(0.0, -30.0), point(4.0, -6.0)]).unwrap();
    project.set_automation(send(pad), vec![point(0.0, 0.0)]).unwrap();
    project.set_automation_enabled(&send(pad), false).unwrap();
    assert!(project.track(keys).unwrap().sends.is_empty() && project.track(pad).unwrap().sends.is_empty());

    // The song as it is, with every part of it in a file.
    let scripted = ScriptedFactory::new();
    let folder = tempfile::tempdir().unwrap();
    let all = RenderOptions { stems: Some(Stems::all(folder.path())), ..options() };
    let whole = render(&project, &scripted, &all);
    let stem = |what: StemKind, track: TrackId| {
        let stem = whole.measured.stems.iter().find(|stem| stem.what == what && stem.track == Some(track)).unwrap();
        read(&stem.path)
    };
    let in_echo = stem(StemKind::Return, echo);
    assert!(peak(&in_echo.0) > 0.005, "the echo has what was sent to it: {}", peak(&in_echo.0));

    // The echo heard alone, with `tracks`, is the echo's part of the whole
    // song: every send that reaches it, moving as it moves there, and no
    // track itself. The drums and the pad, which send nothing, are not
    // even made.
    let fresh = ScriptedFactory::new();
    let alone = render(&project, &fresh, &RenderOptions { only_tracks: Some(vec![echo]), ..options() });
    let off = apart("the echo alone", &in_echo, (&alone.left, &alone.right));
    assert!(off < 1e-6, "{off} from the echo's part of the whole song");
    let made: Vec<String> = fresh.made().into_iter().map(|(uri, _)| uri).collect();
    assert_eq!(made, [BASS], "the drums feed nothing that is heard");
    let rows: Vec<TrackId> = alone.measured.tracks.iter().map(|level| level.track).collect();
    assert_eq!(rows, [echo], "and no track is heard itself");
    // Soloed in the song, it is the same.
    let mut soloed = project.clone();
    soloed.track_mut(echo).unwrap().solo = true;
    let heard = render(&soloed, &scripted, &options());
    assert!(apart("the echo soloed", &in_echo, (&heard.left, &heard.right)) < 1e-6);

    // And the envelopes are what is heard in it: without the keys' throw
    // there is less in the echo, and with the pad's envelope switched on
    // there is more.
    let differs = |other: &Rendered| {
        in_echo.0.iter().zip(&other.left).fold(0.0f32, |most, (a, b)| most.max((a - b).abs())) > 1e-3
    };
    let mut without = project.clone();
    without.set_automation(send(keys), Vec::new()).unwrap();
    let less = render(&without, &scripted, &RenderOptions { only_tracks: Some(vec![echo]), ..options() });
    assert!(differs(&less), "the throw is in the echo");
    let mut with_pad = project.clone();
    with_pad.set_automation_enabled(&send(pad), true).unwrap();
    let more = render(&with_pad, &scripted, &RenderOptions { only_tracks: Some(vec![echo]), ..options() });
    assert!(differs(&more), "and the pad would be, with its envelope on");

    // The keys soloed are heard with the echo, which stays open for a
    // soloed track and then holds the keys' throw and nothing of the bass:
    // what the song without the other tracks renders to.
    let mut soloed = project.clone();
    soloed.track_mut(keys).unwrap().solo = true;
    let heard = render(&soloed, &scripted, &options());
    let mut only_keys = project.clone();
    for other in [bass, pad, drums] {
        only_keys.remove_track(other).unwrap();
    }
    let expected = render(&only_keys, &scripted, &options());
    assert!(apart("the keys soloed", &(heard.left, heard.right), (&expected.left, &expected.right)) < 1e-6);
}

#[test]
fn a_track_that_a_device_listens_to_is_made_for_whatever_is_heard_and_a_muted_return_can_be_a_key() {
    // A pad that a compressor turns down under the kick, and sends to a
    // hall. The kick itself sends nothing there.
    let squeeze = [("threshold", -40.0), ("ratio", 10.0), ("attack", 1.0), ("release", 60.0)];
    let build = || {
        let mut project = Project::new("keyed");
        let hall = project.add_track_of_kind("Hall", TrackKind::Return);
        device(&mut project, ChainRef::Track(hall), "builtin:reverb", &[("mix", 1.0)]);
        let kicks: Vec<Note> = (0..8).map(|beat| note(36, f64::from(beat), 0.25)).collect();
        let kick = part(&mut project, "Kick", "builtin:drumkit", &kicks);
        let pad = part(&mut project, "Pad", "builtin:polysynth", &[note(57, 0.0, 8.0)]);
        let compressor = device(&mut project, ChainRef::Track(pad), "builtin:compressor", &squeeze);
        project.set_send(pad, hall, Some(-6.0)).unwrap();
        (project, hall, kick, pad, compressor)
    };
    let scripted = ScriptedFactory::new();
    let folder = tempfile::tempdir().unwrap();
    let hall_of = |project: &Project, hall: TrackId, name: &str| {
        let stems = Stems { folder: folder.path().join(name), returns: true, ..Stems::default() };
        let whole = render(project, &scripted, &RenderOptions { stems: Some(stems), ..options() });
        let stem = whole.measured.stems.iter().find(|stem| stem.track == Some(hall)).unwrap();
        read(&stem.path)
    };

    // The hall heard alone is the hall of the whole song: the pad that
    // feeds it is made, and so is the kick the pad's compressor listens
    // to, though the kick is neither heard nor sent anywhere.
    let (mut project, hall, kick, pad, compressor) = build();
    let unkeyed = hall_of(&project, hall, "unkeyed");
    project.set_sidechain(compressor, Some(kick)).unwrap();
    let keyed = hall_of(&project, hall, "keyed");
    assert!(apart("the hall with and without the kick as a key", &keyed, (&unkeyed.0, &unkeyed.1)) > 0.01);
    let alone = render(&project, &scripted, &RenderOptions { only_tracks: Some(vec![hall]), ..options() });
    let off = apart("the hall heard alone", &keyed, (&alone.left, &alone.right));
    assert!(off < 1e-6, "{off} from the hall of the whole song");
    let rows: Vec<TrackId> = alone.measured.tracks.iter().map(|level| level.track).collect();
    assert_eq!(rows, [hall], "neither the pad nor the kick is heard");

    // A return that is muted can be what a device listens to: the kick's
    // send still reaches it, and the pad is turned down as by the kick
    // itself. Heard alone, the pad is turned down the same way: what its
    // compressor listens to is fed as in the whole song, by a kick that is
    // not heard.
    let pad_of = |project: &Project, pad: TrackId, name: &str, only: Option<Vec<TrackId>>| {
        let stems = Stems { folder: folder.path().join(name), tracks: true, ..Stems::default() };
        let out = render(project, &scripted, &RenderOptions { stems: Some(stems), only_tracks: only, ..options() });
        let stem = out.measured.stems.iter().find(|stem| stem.track == Some(pad)).unwrap();
        read(&stem.path)
    };
    let by_itself = pad_of(&build().0, pad, "by itself", None);
    let by_the_kick = pad_of(&project, pad, "by the kick", None);
    assert!(apart("the pad with and without a key", &by_the_kick, (&by_itself.0, &by_itself.1)) > 0.01);
    let side = project.add_track_of_kind("Side", TrackKind::Return);
    project.set_send(kick, side, Some(0.0)).unwrap();
    project.track_mut(side).unwrap().mute = true;
    project.set_sidechain(compressor, Some(side)).unwrap();
    let by_the_return = pad_of(&project, pad, "by the return", None);
    let off = apart("the pad under a muted return", &by_the_return, (&by_the_kick.0, &by_the_kick.1));
    assert!(off < 1e-4, "{off} from the pad under the kick itself");
    let alone = pad_of(&project, pad, "alone", Some(vec![pad]));
    let off = apart("the pad heard alone", &alone, (&by_the_return.0, &by_the_return.1));
    assert!(off < 1e-6, "{off} from the pad in the whole song");
}
