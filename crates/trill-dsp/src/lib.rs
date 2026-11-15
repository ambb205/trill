//! The instruments and effects that ship with the DAW, the factory that
//! exposes them under `builtin:<key>` URIs, and offline audio processing
//! (time-stretching, resampling).
//!
//! Most built-in devices are described entirely by their parameters. The
//! sample players ([`sampler`], [`drumrack`]) also need audio files: which
//! ones is stored in the device's state (see [`loading`]), and the files
//! themselves come from an [`AudioLoader`] given to [`Builtins`].

pub mod drumrack;
pub mod drums;
pub mod effects;
pub mod fft;
pub mod loading;
pub mod params;
pub mod playhead;
pub mod resample;
pub mod sample;
pub mod sampler;
pub mod stretch;
pub mod synth;
pub mod util;
pub mod voice;

#[cfg(test)]
mod testing;

pub use loading::{SampleRef, StateError};

use effects::{delay, dynamics, eq, modulation, reverb, utility};
use loading::{SampleController, SampleSource, SampleStore};
use params::{ParamDef, PresetDef};
use std::sync::Arc;
use trill_core::Device;
use trill_core::audio::AudioLoader;
use trill_core::device::{
    BUILTIN_SCHEME, DeviceCategory, DeviceController, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance,
    DeviceProcessor, split_uri,
};

/// Makes a sample player from its stored state, loading its files. Fails
/// only if there is no way to load files at all.
type CreateSampled = fn(Option<&[u8]>, &SampleSource) -> Result<Box<dyn DeviceProcessor>, String>;
/// Lists the files a stored state refers to.
type ListSamples = fn(Option<&[u8]>) -> Result<Vec<SampleRef>, StateError>;

/// How a built-in device is made.
#[derive(Copy, Clone)]
enum Create {
    /// From its parameters alone.
    Plain(fn() -> Box<dyn DeviceProcessor>),
    /// From its state document, which names the audio files it plays.
    Sampled { create: CreateSampled, references: ListSamples },
}

struct Builtin {
    key: &'static str,
    name: &'static str,
    category: DeviceCategory,
    description: &'static str,
    params: &'static [ParamDef],
    presets: &'static [PresetDef],
    create: Create,
}

const fn effect(
    key: &'static str,
    name: &'static str,
    description: &'static str,
    params: &'static [ParamDef],
    create: fn() -> Box<dyn DeviceProcessor>,
) -> Builtin {
    let category = DeviceCategory::Effect;
    Builtin { key, name, category, description, params, presets: &[], create: Create::Plain(create) }
}

static BUILTINS: &[Builtin] = &[
    Builtin {
        key: "polysynth",
        name: "Poly Synth",
        category: DeviceCategory::Instrument,
        description: synth::DESCRIPTION,
        params: synth::PARAMS,
        presets: synth::PRESETS,
        create: Create::Plain(|| Box::new(synth::PolySynth::new())),
    },
    Builtin {
        key: "drumkit",
        name: "Drum Kit",
        category: DeviceCategory::Instrument,
        description: drums::DESCRIPTION,
        params: drums::PARAMS,
        presets: drums::PRESETS,
        create: Create::Plain(|| Box::new(drums::DrumKit::new())),
    },
    Builtin {
        key: "sampler",
        name: "Sampler",
        category: DeviceCategory::Instrument,
        description: sampler::DESCRIPTION,
        params: sampler::PARAMS,
        presets: sampler::PRESETS,
        create: Create::Sampled { create: sampler::create, references: sampler::sample_refs },
    },
    Builtin {
        key: "drumrack",
        name: "Drum Rack",
        category: DeviceCategory::Instrument,
        description: drumrack::DESCRIPTION,
        params: drumrack::PARAMS,
        presets: &[],
        create: Create::Sampled { create: drumrack::create, references: drumrack::sample_refs },
    },
    effect("reverb", "Reverb", reverb::DESCRIPTION, reverb::PARAMS, || Box::new(reverb::Reverb::new())),
    effect("delay", "Delay", delay::DESCRIPTION, delay::PARAMS, || Box::new(delay::Delay::new())),
    effect("eq", "EQ", eq::DESCRIPTION, eq::PARAMS, || Box::new(eq::Eq::new())),
    effect("compressor", "Compressor", dynamics::COMPRESSOR_DESCRIPTION, dynamics::COMPRESSOR_PARAMS, || {
        Box::new(dynamics::Compressor::new())
    }),
    effect("limiter", "Limiter", dynamics::LIMITER_DESCRIPTION, dynamics::LIMITER_PARAMS, || {
        Box::new(dynamics::Limiter::new())
    }),
    effect("chorus", "Chorus", modulation::CHORUS_DESCRIPTION, modulation::CHORUS_PARAMS, || {
        Box::new(modulation::Chorus::new())
    }),
    effect("filter", "Auto Filter", modulation::FILTER_DESCRIPTION, modulation::FILTER_PARAMS, || {
        Box::new(modulation::AutoFilter::new())
    }),
    effect("pump", "Pump", modulation::PUMP_DESCRIPTION, modulation::PUMP_PARAMS, || Box::new(modulation::Pump::new())),
    effect("saturator", "Saturator", utility::SATURATOR_DESCRIPTION, utility::SATURATOR_PARAMS, || {
        Box::new(utility::Saturator::new())
    }),
    effect("utility", "Utility", utility::UTILITY_DESCRIPTION, utility::UTILITY_PARAMS, || {
        Box::new(utility::Utility::new())
    }),
];

fn describe(builtin: &Builtin) -> DeviceDescriptor {
    DeviceDescriptor {
        uri: format!("{BUILTIN_SCHEME}:{}", builtin.key),
        name: builtin.name.to_string(),
        vendor: "Built-in".to_string(),
        category: builtin.category,
        description: builtin.description.to_string(),
        params: builtin.params.iter().map(ParamDef::info).collect(),
        presets: builtin.presets.iter().map(PresetDef::preset).collect(),
    }
}

fn find(uri: &str) -> Option<&'static Builtin> {
    match split_uri(uri) {
        Some((BUILTIN_SCHEME, key)) => BUILTINS.iter().find(|builtin| builtin.key == key),
        _ => None,
    }
}

fn create(
    device: &Device,
    sample_rate: f64,
    max_block: usize,
    source: &SampleSource,
) -> Result<DeviceInstance, DeviceError> {
    let builtin = find(&device.uri).ok_or_else(|| DeviceError::Unknown(device.uri.clone()))?;
    let (mut processor, controller) = match builtin.create {
        Create::Plain(create) => (create(), None),
        Create::Sampled { create, .. } => {
            let processor = create(device.state.as_deref(), source)
                .map_err(|reason| DeviceError::LoadFailed { uri: device.uri.clone(), reason })?;
            let controller: Box<dyn DeviceController> = Box::new(SampleController { state: device.state.clone() });
            (processor, Some(controller))
        }
    };
    processor.prepare(sample_rate, max_block);
    for (index, def) in builtin.params.iter().enumerate() {
        if let Some(value) = device.params.get(def.id) {
            processor.set_param(index, *value);
        }
    }
    Ok(DeviceInstance { processor, params: builtin.params.iter().map(ParamDef::info).collect(), controller })
}

/// Creates the built-in devices, loading the audio files of the sample
/// players through an [`AudioLoader`].
///
/// This is the factory an application should use. Share one instance (it is
/// usually put in an `Arc`) between the session and its offline renders, so
/// that each audio file is prepared for playback once however many devices
/// and renders use it.
#[derive(Default)]
pub struct Builtins {
    loader: Option<Arc<dyn AudioLoader>>,
    samples: SampleStore,
}

impl Builtins {
    /// A factory whose sample players load their files with `loader`.
    pub fn with_loader(loader: Arc<dyn AudioLoader>) -> Self {
        Self { loader: Some(loader), samples: SampleStore::default() }
    }

    /// What is wrong with the samples of `device`, as messages for the
    /// user; empty when all is well. See [`sample_problems`].
    pub fn sample_problems(&self, device: &Device) -> Vec<String> {
        match &self.loader {
            Some(loader) => sample_problems(device, loader.as_ref()),
            None => Vec::new(),
        }
    }
}

impl DeviceFactory for Builtins {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        BUILTINS.iter().map(describe).collect()
    }

    fn create(&self, device: &Device, sample_rate: f64, max_block: usize) -> Result<DeviceInstance, DeviceError> {
        let source = SampleSource { loader: self.loader.as_deref(), store: Some(&self.samples) };
        create(device, sample_rate, max_block, &source)
    }
}

/// Creates the built-in devices without access to audio files.
///
/// Everything that needs no files works as with [`Builtins`], including a
/// sampler or drum rack with nothing loaded. Creating one that does name a
/// file fails with an error that says to use [`Builtins::with_loader`].
pub struct BuiltinFactory;

impl DeviceFactory for BuiltinFactory {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        BUILTINS.iter().map(describe).collect()
    }

    fn create(&self, device: &Device, sample_rate: f64, max_block: usize) -> Result<DeviceInstance, DeviceError> {
        create(device, sample_rate, max_block, &SampleSource { loader: None, store: None })
    }
}

/// The audio files `device` plays, if it is a built-in sample player with a
/// readable state. For collecting a project's files, or finding the devices
/// that use one.
pub fn sample_refs(device: &Device) -> Vec<SampleRef> {
    match find(&device.uri).map(|builtin| builtin.create) {
        Some(Create::Sampled { references, .. }) => references(device.state.as_deref()).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// What is wrong with the samples of `device`, as messages for the user:
/// a state that cannot be read, files that cannot be loaded, entries for
/// pads that do not exist. Empty when all is well, and for devices that are
/// not built-in sample players.
///
/// None of these stop the device from being created: it loads and plays
/// silence where a sample is missing. The engine calls this after creating
/// a device to tell the user. Loaders cache what they load (failures
/// included), so asking again costs little.
pub fn sample_problems(device: &Device, loader: &dyn AudioLoader) -> Vec<String> {
    let Some(Create::Sampled { references, .. }) = find(&device.uri).map(|builtin| builtin.create) else {
        return Vec::new();
    };
    let references = match references(device.state.as_deref()) {
        Ok(references) => references,
        Err(error) => return vec![format!("{error}; the device is empty until its samples are set again")],
    };
    let mut problems = Vec::new();
    for SampleRef { slot, path } in references {
        // A drum rack entry for a note outside the pads is kept in the
        // document but never played.
        let pad = slot.strip_prefix("pad ").and_then(|note| note.parse::<u8>().ok());
        if pad.is_some_and(|note| !drumrack::PAD_NOTES.contains(&note)) {
            problems.push(format!("{slot} does not exist (pads are notes 36-51), so '{path}' is never played"));
        } else if let Err(reason) = loader.load(&path) {
            problems.push(format!("{slot}: cannot load '{path}': {reason}"));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use testing::MemoryLoader;
    use trill_core::device::{Event, EventKind, ProcessInfo};
    use trill_core::{ChainRef, Project};

    const SR: f64 = 48_000.0;
    const BLOCK: usize = 256;

    fn info(position_beats: f64) -> ProcessInfo {
        ProcessInfo {
            sample_rate: SR,
            tempo: 120.0,
            time_signature: Default::default(),
            playing: true,
            position_beats,
            loop_range: None,
        }
    }

    /// A device model for `builtin:<key>`. The sample players get a state
    /// that puts `tone.wav` on the sampler and on every pad of the rack.
    fn model(key: &str, params: &[(&str, f64)]) -> Device {
        let mut project = Project::new("t");
        let id = project.add_device(ChainRef::Master, format!("builtin:{key}"), key, None).unwrap();
        let device = project.device_mut(id).unwrap();
        for (name, value) in params {
            device.params.insert(name.to_string(), *value);
        }
        device.state = match key {
            "sampler" => Some(sampler::State::new("tone.wav").to_bytes()),
            "drumrack" => {
                Some(drumrack::State::with_pads(drumrack::PAD_NOTES.map(|note| (note, "tone.wav"))).unwrap().to_bytes())
            }
            _ => None,
        };
        device.clone()
    }

    /// A factory whose loader knows one file, `tone.wav`: a decaying 220 Hz
    /// tone half a second long.
    fn factory() -> Builtins {
        let loader = MemoryLoader::default();
        let tone = testing::sine(220.0, SR, 24_000);
        loader.insert(
            "tone.wav",
            tone.iter().enumerate().map(|(i, s)| s * 0.5 * (-(i as f32) / 6_000.0).exp()).collect(),
            SR,
        );
        Builtins::with_loader(Arc::new(loader))
    }

    fn make(key: &str, params: &[(&str, f64)]) -> Box<dyn DeviceProcessor> {
        factory().create(&model(key, params), SR, BLOCK).unwrap().processor
    }

    /// Runs `blocks` blocks. `input` fills each block before processing and
    /// `events` supplies the events for block `n`.
    fn run(
        device: &mut dyn DeviceProcessor,
        blocks: usize,
        input: impl Fn(usize) -> f32,
        events: impl Fn(usize) -> Vec<Event>,
    ) -> (Vec<f32>, Vec<f32>) {
        let (mut out_l, mut out_r) = (Vec::new(), Vec::new());
        for block in 0..blocks {
            let mut l: Vec<f32> = (0..BLOCK).map(|i| input(block * BLOCK + i)).collect();
            let mut r = l.clone();
            let beats = (block * BLOCK) as f64 / SR * 2.0;
            device.process(&info(beats), &events(block), &mut l, &mut r);
            out_l.extend(l);
            out_r.extend(r);
        }
        (out_l, out_r)
    }

    fn noise(i: usize) -> f32 {
        let x = (i as u32).wrapping_mul(2_654_435_761).rotate_left(13).wrapping_mul(0x9E37_79B9);
        (x >> 8) as f32 / 8_388_608.0 - 1.0
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
    }

    fn note_on(pitch: u8) -> Vec<Event> {
        vec![Event { offset: 0, kind: EventKind::NoteOn { pitch, velocity: 0.8 } }]
    }

    fn note_off(pitch: u8) -> Vec<Event> {
        vec![Event { offset: 0, kind: EventKind::NoteOff { pitch } }]
    }

    #[test]
    fn preset_values_reference_real_parameters() {
        for builtin in BUILTINS {
            for preset in builtin.presets {
                for (id, value) in preset.params {
                    let def = builtin
                        .params
                        .iter()
                        .find(|def| def.id == *id)
                        .unwrap_or_else(|| panic!("{}/{}: unknown parameter '{id}'", builtin.key, preset.name));
                    assert!(
                        (def.min..=def.max).contains(value),
                        "{}/{}: {id} = {value} is outside {}..={}",
                        builtin.key,
                        preset.name,
                        def.min,
                        def.max
                    );
                }
            }
            let mut ids: Vec<&str> = builtin.params.iter().map(|def| def.id).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), builtin.params.len(), "{}: duplicate parameter id", builtin.key);
            for def in builtin.params {
                assert!(def.min < def.max && (def.min..=def.max).contains(&def.default), "{}/{}", builtin.key, def.id);
            }
        }
    }

    #[test]
    fn every_device_stays_finite_and_bounded() {
        for builtin in BUILTINS {
            let mut device = make(builtin.key, &[]);
            let instrument = builtin.category == DeviceCategory::Instrument;
            let (l, r) = run(
                device.as_mut(),
                200,
                |i| if instrument { 0.0 } else { noise(i) * 0.5 },
                |block| match block % 40 {
                    0 => vec![
                        Event { offset: 0, kind: EventKind::NoteOn { pitch: 36, velocity: 1.0 } },
                        Event { offset: 10, kind: EventKind::NoteOn { pitch: 60, velocity: 1.0 } },
                        Event { offset: 100, kind: EventKind::NoteOn { pitch: 42, velocity: 0.7 } },
                    ],
                    20 => vec![
                        Event { offset: 5, kind: EventKind::NoteOff { pitch: 36 } },
                        Event { offset: 5, kind: EventKind::NoteOff { pitch: 60 } },
                    ],
                    _ => vec![],
                },
            );
            for sample in l.iter().chain(r.iter()) {
                assert!(sample.is_finite(), "{}: non-finite output", builtin.key);
            }
            assert!(peak(&l) < 4.0 && peak(&r) < 4.0, "{}: output too hot ({})", builtin.key, peak(&l));
            assert!(peak(&l) > 0.001, "{}: no output", builtin.key);
        }
    }

    #[test]
    fn every_synth_preset_makes_sound_and_falls_silent() {
        for preset in synth::PRESETS {
            let mut device = make("polysynth", preset.params);
            let (l, _) = run(device.as_mut(), 60, |_| 0.0, |b| if b == 0 { note_on(57) } else { vec![] });
            assert!(peak(&l) > 0.01, "preset {} is silent", preset.name);
            assert!(peak(&l) < 1.5, "preset {} is too loud: {}", preset.name, peak(&l));
            // Release, then wait longer than any preset's release time.
            let blocks = (25.0 * SR / BLOCK as f64) as usize;
            let (tail, _) = run(device.as_mut(), blocks, |_| 0.0, |b| if b == 0 { note_off(57) } else { vec![] });
            assert_eq!(peak(&tail[tail.len() - BLOCK..]), 0.0, "preset {} never goes silent", preset.name);
        }
    }

    #[test]
    fn synth_plays_the_right_pitch() {
        let mut device = make("polysynth", &[("osc1_wave", 0.0), ("velocity_sens", 0.0)]);
        let (l, _) = run(device.as_mut(), 200, |_| 0.0, |b| if b == 0 { note_on(69) } else { vec![] });
        let steady = &l[4800..];
        let crossings = steady.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        let hz = crossings as f64 / (steady.len() as f64 / SR);
        assert!((hz - 440.0).abs() < 1.5, "expected 440 Hz, measured {hz}");
    }

    #[test]
    fn mono_synth_returns_to_the_held_note() {
        let mut device = make("polysynth", &[("osc1_wave", 0.0), ("mono", 1.0), ("velocity_sens", 0.0)]);
        let events = |b: usize| match b {
            0 => note_on(57),
            10 => note_on(69),
            20 => note_off(69),
            _ => vec![],
        };
        let (l, _) = run(device.as_mut(), 220, |_| 0.0, events);
        let steady = &l[30 * BLOCK..];
        let crossings = steady.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        let hz = crossings as f64 / (steady.len() as f64 / SR);
        assert!((hz - 220.0).abs() < 1.5, "expected the held A3 (220 Hz), measured {hz}");
    }

    #[test]
    fn drum_kit_is_silent_until_hit() {
        let mut device = make("drumkit", &[]);
        let (l, _) = run(device.as_mut(), 4, |_| 0.0, |_| vec![]);
        assert_eq!(peak(&l), 0.0);
        for pitch in [36u8, 37, 38, 39, 41, 42, 45, 46, 48, 49, 51, 54, 56, 70, 75] {
            device.reset();
            let (l, _) = run(device.as_mut(), 8, |_| 0.0, |b| if b == 0 { note_on(pitch) } else { vec![] });
            assert!(peak(&l) > 0.02, "drum {pitch} is silent (peak {})", peak(&l));
        }
    }

    #[test]
    fn closed_hat_chokes_open_hat() {
        let mut open = make("drumkit", &[]);
        let (ringing, _) = run(open.as_mut(), 40, |_| 0.0, |b| if b == 0 { note_on(46) } else { vec![] });
        let mut choked = make("drumkit", &[]);
        let events = |b: usize| match b {
            0 => note_on(46),
            10 => note_on(42),
            _ => vec![],
        };
        let (cut, _) = run(choked.as_mut(), 40, |_| 0.0, events);
        let late = 30 * BLOCK..40 * BLOCK;
        assert!(rms(&cut[late.clone()]) < rms(&ringing[late]) * 0.1);
    }

    #[test]
    fn limiter_never_exceeds_its_ceiling() {
        let mut device = make("limiter", &[("ceiling", -1.0), ("gain", 12.0)]);
        // Silence, then a sudden full-scale burst: the hardest case for look-ahead.
        let input = |i: usize| if i < 3000 { 0.0 } else { noise(i) * 2.5 };
        let (l, r) = run(device.as_mut(), 100, input, |_| vec![]);
        let ceiling = util::db_to_gain(-1.0) * 1.0001;
        assert!(peak(&l) <= ceiling && peak(&r) <= ceiling, "peak {} > ceiling {ceiling}", peak(&l));
        assert!(rms(&l[10_000..]) > 0.3, "limiter should be loud, not silent");
    }

    #[test]
    fn limiter_is_transparent_below_the_ceiling() {
        let mut device = make("limiter", &[]);
        let latency = device.latency_samples() as usize;
        let input = |i: usize| noise(i) * 0.5;
        let (l, _) = run(device.as_mut(), 20, input, |_| vec![]);
        for (i, sample) in l.iter().enumerate().skip(latency) {
            assert!((sample - input(i - latency)).abs() < 1e-6, "sample {i} was altered");
        }
    }

    #[test]
    fn delay_echoes_at_the_synced_time() {
        // 1 beat at 120 BPM = 0.5 s = 24000 samples.
        let mut device = make("delay", &[("time_beats", 1.0), ("ping_pong", 0.0), ("feedback", 0.0), ("mix", 1.0)]);
        let (l, _) = run(device.as_mut(), 120, |i| if i == 0 { 1.0 } else { 0.0 }, |_| vec![]);
        let (index, value) =
            l.iter().enumerate().fold((0, 0.0f32), |best, (i, s)| if s.abs() > best.1 { (i, s.abs()) } else { best });
        assert!((index as i64 - 24_000).abs() <= 1, "echo at sample {index}");
        assert!(value > 0.9);
    }

    #[test]
    fn reverb_tail_decays() {
        let mut device = make("reverb", &[("decay", 1.0), ("mix", 1.0)]);
        let (l, r) = run(device.as_mut(), 600, |i| if i < 64 { noise(i) } else { 0.0 }, |_| vec![]);
        let early = rms(&l[2_000..12_000]);
        let late = rms(&l[100_000..110_000]);
        assert!(early > 1e-3, "reverb produced no tail");
        assert!(late < early * 0.05, "tail is not decaying: early {early}, late {late}");
        assert!(rms(&r[2_000..12_000]) > 1e-3);
        let difference: f32 = l.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum();
        assert!(difference > 1.0, "reverb output should be stereo");
    }

    #[test]
    fn compressor_reduces_loud_signals() {
        let mut device = make("compressor", &[("threshold", -30.0), ("ratio", 10.0), ("knee", 0.0)]);
        let (l, _) = run(device.as_mut(), 100, |i| noise(i) * 0.8, |_| vec![]);
        let input_rms = 0.8 / 3f32.sqrt();
        assert!(rms(&l[10_000..]) < input_rms * 0.3);
    }

    #[test]
    fn a_hard_knee_at_exactly_the_threshold_stays_finite() {
        // The level sits exactly on the threshold: the one input for which
        // a knee of zero used to produce 0/0 and silence the track for good.
        let threshold = util::gain_to_db(0.5) as f64;
        let mut device = make("compressor", &[("threshold", threshold), ("knee", 0.0), ("ratio", 4.0)]);
        let (l, r) = run(device.as_mut(), 20, |_| 0.5, |_| vec![]);
        assert!(l.iter().chain(&r).all(|sample| sample.is_finite()));
        assert!((l[l.len() - 1] - 0.5).abs() < 1e-4, "nothing to reduce at the threshold: {}", l[l.len() - 1]);
    }

    #[test]
    fn pump_ducks_on_the_beat() {
        let mut device = make("pump", &[("depth", 1.0)]);
        // 120 BPM: a beat is 24000 samples.
        let (l, _) = run(device.as_mut(), 200, |_| 1.0, |_| vec![]);
        assert!(l[24_000 + 400] < 0.2, "should be ducked just after the beat");
        assert!(l[24_000 + 20_000] > 0.9, "should have recovered before the next beat");
    }

    #[test]
    fn unknown_devices_are_reported() {
        let mut project = Project::new("t");
        let id = project.add_device(ChainRef::Master, "builtin:nope", "x", None).unwrap();
        let device = project.device(id).unwrap();
        assert!(matches!(BuiltinFactory.create(device, SR, BLOCK), Err(DeviceError::Unknown(_))));
        assert!(matches!(factory().create(device, SR, BLOCK), Err(DeviceError::Unknown(_))));
        assert!(matches!(
            factory().create(&Device { uri: "clap:builtin:sampler".into(), ..device.clone() }, SR, BLOCK),
            Err(DeviceError::Unknown(_))
        ));
    }

    #[test]
    fn both_factories_list_the_same_devices_including_the_sample_players() {
        let listed = BuiltinFactory.descriptors();
        assert_eq!(listed, Builtins::default().descriptors());
        assert_eq!(listed, factory().descriptors());
        assert_eq!(listed.len(), BUILTINS.len());
        for (uri, pad_param) in [("builtin:sampler", "loop_mode"), ("builtin:drumrack", "pad36_level")] {
            let descriptor = listed.iter().find(|d| d.uri == uri).unwrap_or_else(|| panic!("{uri} is not listed"));
            assert_eq!(descriptor.category, DeviceCategory::Instrument);
            assert!(descriptor.description.len() > 200 && descriptor.description.contains("device state"));
            assert!(descriptor.params.iter().any(|param| param.id == pad_param));
        }
    }

    /// `BuiltinFactory` is used as a unit struct all over the workspace and
    /// has no loader. It must keep creating everything that needs no files.
    #[test]
    fn the_unit_factory_creates_every_device_that_names_no_file() {
        for builtin in BUILTINS {
            let device = Device { state: None, ..model(builtin.key, &[]) };
            let instance = BuiltinFactory.create(&device, SR, BLOCK).unwrap_or_else(|e| panic!("{}: {e}", builtin.key));
            assert_eq!(instance.params.len(), builtin.params.len());
            let sampled = matches!(builtin.create, Create::Sampled { .. });
            assert_eq!(instance.controller.is_some(), sampled, "{}", builtin.key);
        }
        // An empty sampler is silent, not broken.
        let empty = Device { state: Some(sampler::State::default().to_bytes()), ..model("sampler", &[]) };
        let mut device = BuiltinFactory.create(&empty, SR, BLOCK).unwrap().processor;
        let (l, _) = run(device.as_mut(), 8, |_| 0.0, |b| if b == 0 { note_on(60) } else { vec![] });
        assert_eq!(peak(&l), 0.0);
    }

    #[test]
    fn the_unit_factory_says_how_to_load_samples() {
        for key in ["sampler", "drumrack"] {
            let Err(error) = BuiltinFactory.create(&model(key, &[]), SR, BLOCK) else {
                panic!("{key}: created a device that needs a file without a loader")
            };
            let DeviceError::LoadFailed { uri, reason } = &error else { panic!("{key}: {error}") };
            assert_eq!(uri, &format!("builtin:{key}"));
            assert!(reason.contains("tone.wav") && reason.contains("Builtins::with_loader"), "{reason}");
            // The same goes for a `Builtins` that was given no loader.
            assert!(matches!(
                Builtins::default().create(&model(key, &[]), SR, BLOCK),
                Err(DeviceError::LoadFailed { .. })
            ));
        }
    }

    #[test]
    fn missing_and_unreadable_samples_do_not_fail_creation_and_are_reported() {
        let factory = factory();
        // Device, state, the problems to expect, and whether note 36 sounds.
        let gone = "sample: cannot load 'gone.wav': no such file";
        let rack = drumrack::State::with_pads([(36, "tone.wav"), (38, "gone.wav"), (42, "lost.wav")]).unwrap();
        let stray = br#"{"version": 1, "pads": [{"note": 36, "path": "tone.wav"}, {"note": 90, "path": "tone.wav"}]}"#;
        type Case<'a> = (&'a str, Option<Vec<u8>>, &'a [&'a str], bool);
        let cases: [Case; 8] = [
            ("sampler", Some(sampler::State::new("tone.wav").to_bytes()), &[], true),
            ("sampler", None, &[], false),
            ("sampler", Some(sampler::State::new("gone.wav").to_bytes()), &[gone], false),
            ("sampler", Some(b"{ not json".to_vec()), &["not a valid document"], false),
            ("sampler", Some(br#"{"version": 99, "path": "tone.wav"}"#.to_vec()), &["newer version (format 99"], false),
            // The pads whose files are there still play.
            (
                "drumrack",
                Some(rack.to_bytes()),
                &["pad 38: cannot load 'gone.wav'", "pad 42: cannot load 'lost.wav'"],
                true,
            ),
            ("drumrack", Some(stray.to_vec()), &["pad 90 does not exist"], true),
            ("drumrack", Some(b"[]".to_vec()), &["not a valid document"], false),
        ];
        for (key, state, expected, sounds) in cases {
            let device = Device { state: state.clone(), ..model(key, &[]) };
            let instance = factory.create(&device, SR, BLOCK).unwrap_or_else(|e| panic!("{key}: creation failed: {e}"));
            let problems = factory.sample_problems(&device);
            assert_eq!(problems.len(), expected.len(), "{key}: {problems:?}");
            for (problem, expected) in problems.iter().zip(expected) {
                assert!(problem.contains(expected), "{key}: '{problem}' should mention '{expected}'");
            }
            // Whatever the state was, it is handed back untouched on save,
            // so a project with a problem loses nothing by being saved.
            assert_eq!(instance.controller.unwrap().save_state(), state);

            // The device runs: silent where its samples are missing.
            let mut processor = instance.processor;
            let (l, _) = run(processor.as_mut(), 8, |_| 0.0, |b| if b == 0 { note_on(36) } else { vec![] });
            assert_eq!(peak(&l) > 0.01, sounds, "{key} with problems {expected:?}: peak {}", peak(&l));
        }
        // Devices that play no samples have nothing to report.
        assert!(factory.sample_problems(&model("polysynth", &[])).is_empty());
        assert!(sample_refs(&model("reverb", &[])).is_empty());
        assert_eq!(sample_refs(&model("sampler", &[])), [SampleRef { slot: "sample".into(), path: "tone.wav".into() }]);
        assert_eq!(sample_refs(&model("drumrack", &[])).len(), drumrack::PAD_COUNT);
    }

    #[test]
    fn devices_playing_the_same_file_share_one_prepared_sample() {
        let loader = Arc::new(MemoryLoader::default());
        loader.insert("tone.wav", vec![0.25; 48_000], SR);
        let factory = Builtins::with_loader(loader.clone());
        let buffer = loader.load("tone.wav").unwrap();
        let before = Arc::strong_count(&buffer);
        // A sampler and a full rack, twice over (as a session and an
        // offline render would): the file's audio is not copied per device
        // by the loader, and the factory prepares it only once.
        let devices: Vec<DeviceInstance> = ["sampler", "drumrack", "sampler", "drumrack"]
            .iter()
            .map(|key| factory.create(&model(key, &[]), SR, BLOCK).unwrap())
            .collect();
        assert_eq!(Arc::strong_count(&buffer), before, "devices keep the prepared sample, not the loader's buffer");
        assert_eq!(factory.samples.holders(), [("tone.wav".to_string(), 2 + 2 * drumrack::PAD_COUNT)]);
        drop(devices);
        assert_eq!(factory.samples.holders(), [("tone.wav".to_string(), 0)]);
    }
}
