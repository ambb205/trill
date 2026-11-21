//! A made-up session of recording, for pictures and tests of what the
//! window shows of an input, of takes and of the round trip.
//!
//! A picture run opens nothing of the machine's hardware, least of all a
//! microphone. A scene puts the engine's scripted devices in its place: an
//! interface whose input hears a voice that is worked out here, a phrase at
//! a time. Everything else is the real thing: the takes are recorded by the
//! session through the scripted input, written to disk by the thread that
//! writes takes, landed in lanes and put together by the song's own edits.
//! The scripted device has no clock of its own, so the scene is the same
//! on every machine: it is advanced here, by hand, and stands still once
//! the window is up.

use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::{ChainRef, Monitoring, Note, TrackId, TrackInput};
use trill_engine::host::scripted::{Hears, ScriptedDevice, ScriptedHost, ScriptedInput};
use trill_engine::{AudioConfig, Measurement};
use trill_runtime::{Parts, Runtime};

/// What the made-up session is in the middle of.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RecordingScene {
    /// Three takes of a voice over a loop, a stretch of the first put on
    /// the track, the lanes unfolded, the track armed and heard.
    Takes,
    /// The same, with a fourth take being recorded.
    Rolling,
    /// The Audio and MIDI panel with the input's level on show and a
    /// measurement of the round trip that has just come back.
    Latency,
}

const INTERFACE: &str = "Studio Interface";
/// The track the scene records on.
pub const VOICE: &str = "Voice";
/// The stretch the takes are recorded over, in beats.
const LOOP: (f64, f64) = (8.0, 16.0);
/// Frames the scripted device is advanced by at a time.
const STEP: usize = 9_600;

/// A voice, more or less: a few harmonics of a note that moves from
/// phrase to phrase, each phrase swelling and dying away, with breaths
/// between. `seconds` is the device's own time, so every take hears
/// another stretch of it.
fn voice(channel: usize, seconds: f64) -> f32 {
    const PHRASE: f64 = 1.3;
    const NOTES: [f64; 7] = [220.0, 246.9, 261.6, 293.7, 329.6, 293.7, 246.9];
    let phrase = (seconds / PHRASE).floor();
    let within = seconds / PHRASE - phrase;
    let note = NOTES[(phrase as usize * 3 + 1) % NOTES.len()];
    // A phrase of three syllables, the middle one the loudest.
    let syllable = (within * 3.0).fract();
    let swell = (std::f64::consts::PI * syllable).sin().max(0.0).powf(0.7);
    let breath = if within > 0.86 { 0.0 } else { 1.0 };
    let stress = [0.7, 1.0, 0.55][((within * 3.0) as usize).min(2)];
    let vibrato = 1.0 + 0.006 * (seconds * std::f64::consts::TAU * 5.3).sin();
    let phase = seconds * std::f64::consts::TAU * note * vibrato;
    let tone = phase.sin() + 0.45 * (2.0 * phase).sin() + 0.2 * (3.0 * phase + 0.4).sin() + 0.08 * (5.0 * phase).sin();
    // The second channel of the interface has nothing plugged in.
    let level = if channel == 0 { 0.2 } else { 0.0 };
    (level * swell * breath * stress * tone) as f32
}

impl RecordingScene {
    pub const ALL: [(&'static str, RecordingScene); 3] =
        [("takes", RecordingScene::Takes), ("rolling", RecordingScene::Rolling), ("latency", RecordingScene::Latency)];

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(known, _)| *known == name).map(|(_, scene)| *scene)
    }

    /// What stands in for the machine's audio devices, and the hand that
    /// advances them.
    pub fn open(self) -> (Parts, Staging) {
        let host = ScriptedHost::with_speakers();
        host.add_device(ScriptedDevice { shared_latency: 960, ..ScriptedDevice::new(INTERFACE) });
        host.add_input(ScriptedInput { latency: 192, unsaid: 96, shares_clock: true, ..ScriptedInput::new(INTERFACE) });
        // More lies between the output's buffers and its speaker than it says.
        host.set_output_unsaid(288);
        host.hear(Hears::Sound(Arc::new(voice)));
        let parts = Parts { audio: Some(Arc::new(host.clone())), ..Parts::default() };
        (parts, Staging { scene: self, host })
    }

    /// What arrives on each channel of the input in the scene: the same
    /// figures every time, since a picture is of no particular moment.
    pub fn levels(self) -> Vec<(f32, bool)> {
        match self {
            RecordingScene::Takes => vec![(0.19, false), (0.0, false)],
            RecordingScene::Rolling => vec![(0.42, true), (0.0, false)],
            RecordingScene::Latency => vec![(0.11, false), (0.09, false)],
        }
    }
}

/// A scene's devices once the program runs, for playing it out.
pub struct Staging {
    scene: RecordingScene,
    host: ScriptedHost,
}

impl Staging {
    /// Advances the scripted device by `frames`, keeping the session's
    /// chores done, and no further ahead of the thread that writes a take
    /// than that thread can make up: it keeps the machine's time.
    fn roll(&self, runtime: &Runtime, frames: usize) {
        let written = |runtime: &Runtime| {
            let session = runtime.session.lock();
            session.growing_takes().iter().map(|take| take.file.frames()).min()
        };
        let before = written(runtime).unwrap_or(0);
        let mut rolled = 0;
        while rolled < frames {
            let step = STEP.min(frames - rolled);
            self.host.advance(step);
            rolled += step;
            runtime.session.lock().housekeeping();
            let deadline = Instant::now() + Duration::from_secs(10);
            while written(runtime).is_some_and(|frames| (frames - before) as usize + 96_000 < rolled)
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// Plays the scene out on the song that is open: the track, its takes
    /// and what is made of them. Returns the track that was recorded on.
    pub fn play(&self, runtime: &Runtime) -> Option<TrackId> {
        let chosen = AudioConfig { output: Some(INTERFACE.to_owned()), ..AudioConfig::default() };
        let _ = runtime.set_audio(chosen);
        let session = &runtime.session;
        let rate = session.lock().audio_info().sample_rate;
        let frames_of = |beats: f64| {
            let tempo = session.lock().project().tempo_map();
            tempo.frames_between(LOOP.0, LOOP.0 + beats, rate).round() as usize
        };
        if session.lock().project().tracks.is_empty() {
            self.backing(runtime);
        }
        let voice = session
            .lock()
            .edit(|project| {
                let voice = project.add_track(VOICE);
                let input = TrackInput { channel: 1, stereo: false, monitor: Monitoring::Armed };
                project.set_track_input(voice, Some(input))?;
                project.set_loop(true, LOOP.0, LOOP.1)?;
                Ok(voice)
            })
            .ok()?;
        {
            let mut session = session.lock();
            session.set_armed(voice, true).ok()?;
            session.ready_input().ok()?;
            session.set_count_in(0);
        }
        // The input is read for a moment before anything is asked of it.
        self.roll(runtime, 48_000);
        if self.scene == RecordingScene::Latency {
            self.measure(runtime);
            return Some(voice);
        }

        // Three passes of the loop, and a little of a fourth that is no take.
        session.lock().seek(LOOP.0);
        session.lock().record().ok()?;
        self.roll(runtime, frames_of(3.0 * (LOOP.1 - LOOP.0) + 0.4));
        session.lock().stop();
        // A stretch of the first take, in the middle of the last.
        let first = session.lock().project().track(voice).ok()?.takes.first().map(|take| take.clip.id)?;
        session.lock().edit(|project| project.comp_take(first, LOOP.0 + 3.0, LOOP.0 + 5.0)).ok()?;

        if self.scene == RecordingScene::Rolling {
            // A fourth take, through the song this time, a few bars in.
            session.lock().edit(|project| project.set_loop(false, LOOP.0, LOOP.1)).ok()?;
            session.lock().seek(LOOP.0);
            session.lock().record().ok()?;
            self.roll(runtime, frames_of(5.3));
        } else {
            session.lock().seek(LOOP.0 + 2.5);
        }
        Some(voice)
    }

    /// Something to sing to, where the song is empty: keys that hold four
    /// chords, two bars each, and a bass under them.
    fn backing(&self, runtime: &Runtime) {
        const CHORDS: [[u8; 3]; 4] = [[57, 60, 64], [53, 57, 60], [55, 60, 64], [55, 59, 62]];
        let mut session = runtime.session.lock();
        session.begin_gesture();
        let _ = session.edit(|project| {
            project.name = "Late Take".to_owned();
            project.set_tempo(96.0)
        });
        for (name, preset, volume, low) in [("Keys", "warm_pad", -9.0, false), ("Bass", "bass", -6.0, true)] {
            let Ok(track) = session.edit(|project| {
                let track = project.add_track(name);
                project.set_track_volume(track, volume)?;
                Ok(track)
            }) else {
                continue;
            };
            let _ = session.add_device(ChainRef::Track(track), "builtin:polysynth", Some(preset), None);
            let notes: Vec<Note> = (CHORDS.iter().enumerate())
                .flat_map(|(bar, chord)| {
                    let start = bar as f64 * 8.0;
                    let held = |pitch: u8| Note { pitch, start, length: 7.5, velocity: 84, ..Default::default() };
                    match low {
                        true => vec![held(chord[0] - 24)],
                        false => chord.iter().map(|pitch| held(*pitch)).collect(),
                    }
                })
                .collect();
            let _ = session.edit(|project| {
                let clip = project.add_midi_clip(track, if low { "Roots" } else { "Chords" }, 0.0, 32.0)?;
                project.clip_mut(clip)?.set_notes(notes)
            });
        }
        session.end_gesture();
    }

    /// Has the input hear what is played, as through a cable, and runs
    /// the measurement of the round trip to its end.
    fn measure(&self, runtime: &Runtime) {
        self.host.hear(Hears::Output { gain: 0.5 });
        let session = &runtime.session;
        // Long enough open for its clock to be known.
        self.roll(runtime, 5 * 48_000);
        if session.lock().measure_latency(1).is_err() {
            return;
        }
        for _ in 0..40 {
            self.roll(runtime, 24_000);
            if !matches!(session.lock().measurement(), Measurement::Running) {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenes_are_found_by_name_and_the_voice_is_quiet_and_phrased() {
        for (name, scene) in RecordingScene::ALL {
            assert_eq!(RecordingScene::named(name), Some(scene));
            assert_eq!(scene.levels().len(), 2);
        }
        assert_eq!(RecordingScene::named("stadium"), None);
        let heard: Vec<f32> = (0..96_000).map(|frame| voice(0, frame as f64 / 48_000.0)).collect();
        let peak = heard.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak > 0.1 && peak < 0.4, "about a quarter of full scale at the most: {peak}");
        // A breath ends every phrase: silence for a tenth of a second.
        let breath = &heard[(1.15 * 48_000.0) as usize..(1.29 * 48_000.0) as usize];
        assert!(breath.iter().all(|sample| *sample == 0.0));
        assert!((0..48_000).all(|frame| voice(1, frame as f64 / 48_000.0) == 0.0), "nothing is plugged in there");
    }
}
