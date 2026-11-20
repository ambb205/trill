//! Notes, wheels and controllers on MIDI channels, for an instrument that
//! gives its channels meanings of their own (an organ's pedalboard, a
//! guitar with a string to a channel).
//!
//! What a track's instrument is told says its channel: a key comes up on
//! the channel it went down on, the same key on two channels is two notes,
//! and whatever the song remembers for a track it remembers by channel:
//! the keys that are down, where each controller stands, what is said
//! again where a song starts in its middle and what is let go of where it
//! stops. Live, through an instrument that writes down what it is told;
//! and in renders, through one that plays each channel as a string of its
//! own (`scripted:strings`), where a stretch rendered alone is held to the
//! same stretch cut from a render of the whole song.

mod rig;

use rig::*;
use std::sync::atomic::Ordering;
use trill_core::device::{CompositeFactory, EventKind};
use trill_core::project::{AutomationPoint, Segment};
use trill_core::{ChainRef, Channel, ControllerKind, Note, Project, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::render::{RenderOptions, Rendered, render};
use trill_engine::scripted::{STRINGS, ScriptedFactory};

const BEAT: u64 = 24_000;

fn channel(number: u8) -> Channel {
    Channel::new(number).unwrap()
}

fn note(pitch: u8, start: f64, length: f64) -> Note {
    Note { pitch, start, length, velocity: 100, ..Default::default() }
}

fn held(beat: f64, value: f64) -> AutomationPoint {
    AutomationPoint { beat, value, shape: Segment::Hold }
}

/// A probe's track with one clip of sixteen beats.
fn part(rig: &mut Rig, notes: &[Note], curves: &[(Channel, ControllerKind, Vec<AutomationPoint>)]) -> TrackId {
    let track = rig.probe_track("Guitar");
    rig.session
        .edit(|project| {
            let clip = project.add_midi_clip(track, "part", 0.0, 16.0)?;
            let clip = project.clip_mut(clip)?;
            clip.add_notes(notes)?;
            for (channel, kind, points) in curves {
                clip.set_controller_on(*channel, *kind, points.clone())?;
            }
            Ok(())
        })
        .unwrap();
    rig.run(2);
    rig.probes.take();
    track
}

/// Starts the song where it stands, runs `buffers` buffers and returns
/// what the probe was told: the frame counted from the song's first, the
/// channel's number and what it was.
fn started(rig: &mut Rig, buffers: usize) -> Vec<(u64, u8, EventKind)> {
    rig.run(1);
    rig.probes.take();
    let start = rig.probes.frames.load(Ordering::Relaxed);
    rig.session.play();
    rig.run(buffers);
    rig.probes.events_on().into_iter().map(|(frame, channel, kind)| (frame - start, channel, kind)).collect()
}

/// The notes among `events`, in order.
fn notes_of(events: &[(u64, u8, EventKind)]) -> Vec<(u64, u8, EventKind)> {
    let is_note = |kind: &EventKind| matches!(kind, EventKind::NoteOn { .. } | EventKind::NoteOff { .. });
    events.iter().copied().filter(|(_, _, kind)| is_note(kind)).collect()
}

/// The keys that are down after `events`, as `(channel, pitch)`, with
/// "all notes off" letting go of every channel's.
fn down_after(events: &[(u64, u8, EventKind)]) -> Vec<(u8, u8)> {
    let mut down: Vec<(u8, u8)> = Vec::new();
    for (_, channel, kind) in events {
        match *kind {
            EventKind::NoteOn { pitch, .. } => down.push((*channel, pitch)),
            EventKind::NoteOff { pitch } => {
                let at = down.iter().position(|key| *key == (*channel, pitch));
                at.map(|at| down.remove(at)).unwrap_or_else(|| panic!("a key came up that was not down: {events:?}"));
            }
            EventKind::AllNotesOff => down.clear(),
            _ => {}
        }
    }
    down
}

#[test]
fn one_key_on_two_channels_is_two_notes_and_each_comes_up_on_the_channel_it_went_down_on() {
    let mut rig = Rig::new();
    let (second, third) = (channel(2), channel(3));
    part(
        &mut rig,
        &[
            // One key held on the first channel while the second and the
            // third play it too.
            note(60, 0.0, 2.0),
            note(60, 0.5, 0.5).on(second),
            note(60, 1.0, 2.0).on(third),
            // A key struck again on its own channel while it sounds.
            note(62, 4.0, 2.0).on(second),
            note(62, 5.0, 0.5).on(second),
        ],
        &[],
    );
    let events = started(&mut rig, 7 * 50);
    assert_eq!(
        notes_of(&events),
        [
            (0, 1, on(60)),
            (BEAT / 2, 2, on(60)),
            (BEAT, 2, off(60)),
            (BEAT, 3, on(60)),
            // The first channel's key comes up: the third's goes on.
            (2 * BEAT, 1, off(60)),
            (3 * BEAT, 3, off(60)),
            (4 * BEAT, 2, on(62)),
            // Struck again on the channel it sounds on, it is let go of
            // first, there and nowhere else.
            (5 * BEAT, 2, off(62)),
            (5 * BEAT, 2, on(62)),
            (5 * BEAT + BEAT / 2, 2, off(62)),
        ]
    );
    assert_eq!(down_after(&events), []);
}

#[test]
fn stopping_and_jumping_let_go_of_the_notes_of_every_channel() {
    let mut rig = Rig::new();
    let (second, sixth) = (channel(2), channel(6));
    part(&mut rig, &[note(60, 0.0, 4.0), note(60, 0.0, 4.0).on(second), note(40, 0.5, 8.0).on(sixth)], &[]);
    // A beat and a half in, three keys are down on three channels.
    let mut events = started(&mut rig, 75);
    assert_eq!(down_after(&events), [(1, 60), (2, 60), (6, 40)]);
    // Stopped: every one of them is let go of.
    rig.session.stop();
    rig.run(2);
    events.extend(rig.probes.events_on());
    assert_eq!(down_after(&events), [], "{events:?}");

    // Playing again from the top and jumping ahead while the keys are
    // down: the same.
    rig.session.seek(0.0);
    let mut events = started(&mut rig, 75);
    assert_eq!(down_after(&events), [(1, 60), (2, 60), (6, 40)]);
    rig.session.seek(12.0);
    rig.run(2);
    events.extend(rig.probes.events_on());
    assert_eq!(down_after(&events), [], "{events:?}");
}

#[test]
fn the_notes_of_a_song_come_up_each_on_its_channel_while_a_key_is_held_by_hand() {
    let mut rig = Rig::new();
    let third = channel(3);
    let track = part(&mut rig, &[note(60, 0.0, 4.0).on(third), note(64, 0.0, 4.0)], &[]);
    rig.session.set_armed(track, true).unwrap();
    let mut events = started(&mut rig, 25);
    // A key held by hand, which is on the first channel.
    let now = rig.now();
    rig.key(now + 480.0, on(72));
    rig.run(4);
    events.extend(rig.probes.events_on());
    assert_eq!(down_after(&events), [(3, 60), (1, 64), (1, 72)]);
    // The song jumps ahead: its notes come up, each on its own channel,
    // and the hand's key stays down.
    rig.session.seek(8.0);
    rig.run(2);
    let jumped = rig.probes.events_on();
    let ups: Vec<(u8, EventKind)> = jumped.iter().map(|(_, channel, kind)| (*channel, *kind)).collect();
    assert_eq!(ups, [(1, off(64)), (3, off(60))], "the song's own, the channels in order");
    events.extend(jumped);
    assert_eq!(down_after(&events), [(1, 72)]);
    // Stopped, everything is let go of, the hand's key with the rest.
    rig.session.stop();
    rig.run(2);
    events.extend(rig.probes.events_on());
    assert_eq!(down_after(&events), []);
}

#[test]
fn a_song_that_starts_in_its_middle_says_where_each_channels_controllers_stand_on_that_channel() {
    let mut rig = Rig::new();
    let (second, sixth) = (channel(2), channel(6));
    let wheel = vec![AutomationPoint::new(1.0, 0.0), AutomationPoint::new(2.0, 1.0)];
    let part_curves = [
        // The second channel's wheel goes up over the second beat and
        // stays; its pedal is down from beat 1 to beat 12.
        (second, ControllerKind::PitchBend, wheel),
        (second, ControllerKind::SUSTAIN, vec![held(1.0, 1.0), held(12.0, 0.0)]),
        // The sixth has a wheel of its own, down a half, and an
        // expression that changes at beat 6.
        (sixth, ControllerKind::PitchBend, vec![held(0.0, -0.5)]),
        (sixth, ControllerKind::EXPRESSION, vec![held(0.0, 0.25), held(6.0, 0.75)]),
    ];
    part(&mut rig, &[note(60, 8.0, 1.0).on(second), note(40, 8.0, 1.0).on(sixth)], &part_curves);

    // Put at beat 8 while it stands still and started there: before its
    // first notes the instrument is told where every controller stands,
    // each on its own channel, on the song's first frame.
    rig.session.stop();
    rig.run(2);
    rig.session.seek(8.0);
    rig.run(2);
    let stood = rig.probes.events_on();
    let events = started(&mut rig, 4);
    let said: Vec<(u8, EventKind)> = (stood.iter().map(|(_, channel, kind)| (*channel, *kind)))
        .chain(events.iter().filter(|(frame, ..)| *frame == 0).map(|(_, channel, kind)| (*channel, *kind)))
        .collect();
    let wheel = |value: f32| EventKind::PitchBend { value };
    let told = |channel: u8, kind: EventKind| {
        // What it was told last of that controller on that channel, before its first note.
        let same = |other: &EventKind| std::mem::discriminant(other) == std::mem::discriminant(&kind);
        let last = said.iter().rev().find(|(on, other)| {
            *on == channel
                && same(other)
                && match (other, &kind) {
                    (EventKind::Control { controller: a, .. }, EventKind::Control { controller: b, .. }) => a == b,
                    _ => true,
                }
        });
        last.map(|(_, kind)| *kind)
    };
    assert_eq!(told(2, wheel(0.0)), Some(wheel(1.0)), "{said:?}");
    assert_eq!(told(6, wheel(0.0)), Some(wheel(-0.5)), "{said:?}");
    assert_eq!(told(2, pedal(true)), Some(pedal(true)), "{said:?}");
    let expression = EventKind::Control { controller: 11, value: 0.75 };
    assert_eq!(told(6, expression), Some(expression), "{said:?}");
    // Nothing of one channel is said on another.
    assert_eq!(told(1, wheel(0.0)), None, "{said:?}");
    assert_eq!(told(6, pedal(true)), None, "{said:?}");
    assert_eq!(told(2, expression), None, "{said:?}");
    // And the notes follow on their own channels.
    let first: Vec<(u8, EventKind)> = notes_of(&events)
        .iter()
        .filter(|(frame, ..)| *frame == 0)
        .map(|(_, channel, kind)| (*channel, *kind))
        .collect();
    assert_eq!(first, [(6, on(40)), (2, on(60))]);
}

// ---- rendered ----------------------------------------------------------------

const RATE: u32 = 48_000;

/// A song for an instrument with a string to a channel.
fn strings_song(notes: &[Note], curves: &[(Channel, ControllerKind, Vec<AutomationPoint>)]) -> Project {
    let mut project = Project::new("strings");
    let track = project.add_track("Guitar");
    project.add_device(ChainRef::Track(track), STRINGS, "Strings", None).unwrap();
    let clip = project.add_midi_clip(track, "part", 0.0, 16.0).unwrap();
    let clip = project.clip_mut(clip).unwrap();
    clip.add_notes(notes).unwrap();
    for (channel, kind, points) in curves {
        clip.set_controller_on(*channel, *kind, points.clone()).unwrap();
    }
    project
}

fn rendered(project: &Project, (start, end): (f64, f64), threads: Option<usize>) -> Rendered {
    let options = RenderOptions {
        sample_rate: RATE,
        start_beat: start,
        end_beat: Some(end),
        max_tail_seconds: 0.0,
        threads,
        ..RenderOptions::default()
    };
    let factory = CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(ScriptedFactory::new())]);
    render(project, &factory, &options).unwrap()
}

/// How much of `audio` is a tone of `hertz`: the level of that tone alone
/// that would measure the same, a tone at full scale being 1.
fn tone(audio: &[f32], hertz: f64) -> f64 {
    let turn = std::f64::consts::TAU * hertz / f64::from(RATE);
    let (mut real, mut imaginary) = (0.0f64, 0.0f64);
    for (frame, sample) in audio.iter().enumerate() {
        let angle = turn * frame as f64;
        real += f64::from(*sample) * angle.cos();
        imaginary += f64::from(*sample) * angle.sin();
    }
    2.0 * (real * real + imaginary * imaginary).sqrt() / audio.len() as f64
}

fn hertz(pitch: f64) -> f64 {
    440.0 * ((pitch - 69.0) / 12.0).exp2()
}

/// The most two stretches of samples differ by.
fn apart(one: &[f32], other: &[f32]) -> f32 {
    assert_eq!(one.len(), other.len());
    one.iter().zip(other).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max)
}

#[test]
fn the_wheel_of_one_channel_bends_that_channels_notes_and_no_others() {
    let (second, third) = (channel(2), channel(3));
    // An A on the second string and the E above it on the third, held;
    // from beat 4 the second string's wheel is all the way up.
    let song = strings_song(
        &[note(69, 0.0, 8.0).on(second), note(76, 0.0, 8.0).on(third)],
        &[(second, ControllerKind::PitchBend, vec![held(0.0, 0.0), held(4.0, 1.0)])],
    );
    let out = rendered(&song, (0.0, 8.0), None);
    let beat = BEAT as usize;
    let (before, after) = (&out.left[beat..3 * beat], &out.left[5 * beat..7 * beat]);
    // Before: an A and an E. After: the A is a B, two semitones up, and
    // the E is where it was.
    let level = tone(before, hertz(76.0));
    assert!(level > 0.03, "the third string sounds: {level}");
    for (stretch, a, b, e, e_bent) in [(before, true, false, true, false), (after, false, true, true, false)] {
        let heard =
            [hertz(69.0), hertz(71.0), hertz(76.0), hertz(78.0)].map(|hertz| tone(stretch, hertz) > level / 2.0);
        assert_eq!(heard, [a, b, e, e_bent], "A, B, E and F sharp");
    }
}

#[test]
fn a_stretch_rendered_alone_is_that_stretch_of_the_whole_song_where_a_channels_wheel_moved_before_it() {
    let (second, third, sixth) = (channel(2), channel(3), channel(6));
    let song = strings_song(
        &[
            // One string is sounding where the stretch starts.
            note(52, 3.0, 4.0).on(sixth),
            note(69, 4.0, 1.0).on(second),
            note(76, 4.5, 1.5).on(third),
            note(69, 5.5, 2.0).on(second),
            note(64, 6.0, 1.0),
        ],
        &[
            // Before the stretch: the second string is bent up and stays;
            // the third is made quiet; the sixth's pedal goes down.
            (second, ControllerKind::PitchBend, vec![AutomationPoint::new(1.0, 0.0), AutomationPoint::new(2.0, 0.75)]),
            (third, ControllerKind::EXPRESSION, vec![held(0.0, 1.0), held(1.5, 0.4)]),
            (sixth, ControllerKind::SUSTAIN, vec![held(2.0, 1.0)]),
            // Inside it: the sixth string's wheel and pressure move.
            (sixth, ControllerKind::PitchBend, vec![AutomationPoint::new(5.0, 0.0), AutomationPoint::new(7.0, -1.0)]),
            (sixth, ControllerKind::Pressure, vec![AutomationPoint::new(4.0, 0.0), AutomationPoint::new(8.0, 1.0)]),
        ],
    );
    let whole = rendered(&song, (0.0, 8.0), None);
    let alone = rendered(&song, (4.0, 8.0), None);
    let beat = BEAT as usize;
    assert_eq!((whole.left.len(), alone.left.len()), (8 * beat, 4 * beat));
    let cut = &whole.left[4 * beat..];
    assert!(cut.iter().any(|sample| sample.abs() > 0.02));
    // The same notes on the same strings, each bent and as loud as the
    // song has it there: to the sample.
    assert_eq!(apart(&alone.left, cut), 0.0);
    assert_eq!(apart(&alone.right, &whole.right[4 * beat..]), 0.0);
    // The second string plays its A three quarters of the way to a B.
    let second_string = hertz(69.0 + 1.5);
    assert!(tone(&alone.left[..beat], second_string) > 0.03, "{}", tone(&alone.left[..beat], second_string));

    // And on any number of threads.
    for threads in [1, 2, 4] {
        let again = rendered(&song, (0.0, 8.0), Some(threads));
        assert_eq!(again.left, whole.left, "on {threads} threads");
    }
}
