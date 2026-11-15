//! The instruments and effects that ship with the DAW, and the factory that
//! exposes them under `builtin:<key>` URIs.

pub mod drums;
pub mod effects;
pub mod params;
pub mod synth;
pub mod util;

use daw_core::Device;
use daw_core::device::{
    BUILTIN_SCHEME, DeviceCategory, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, DeviceProcessor,
    split_uri,
};
use effects::{delay, dynamics, eq, modulation, reverb, utility};
use params::{ParamDef, PresetDef};

struct Builtin {
    key: &'static str,
    name: &'static str,
    category: DeviceCategory,
    description: &'static str,
    params: &'static [ParamDef],
    presets: &'static [PresetDef],
    create: fn() -> Box<dyn DeviceProcessor>,
}

const fn effect(
    key: &'static str,
    name: &'static str,
    description: &'static str,
    params: &'static [ParamDef],
    create: fn() -> Box<dyn DeviceProcessor>,
) -> Builtin {
    Builtin { key, name, category: DeviceCategory::Effect, description, params, presets: &[], create }
}

static BUILTINS: &[Builtin] = &[
    Builtin {
        key: "polysynth",
        name: "Poly Synth",
        category: DeviceCategory::Instrument,
        description: synth::DESCRIPTION,
        params: synth::PARAMS,
        presets: synth::PRESETS,
        create: || Box::new(synth::PolySynth::new()),
    },
    Builtin {
        key: "drumkit",
        name: "Drum Kit",
        category: DeviceCategory::Instrument,
        description: drums::DESCRIPTION,
        params: drums::PARAMS,
        presets: drums::PRESETS,
        create: || Box::new(drums::DrumKit::new()),
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

/// Creates the built-in devices.
pub struct BuiltinFactory;

impl DeviceFactory for BuiltinFactory {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        BUILTINS.iter().map(describe).collect()
    }

    fn create(&self, device: &Device, sample_rate: f64, max_block: usize) -> Result<DeviceInstance, DeviceError> {
        let unknown = || DeviceError::Unknown(device.uri.clone());
        let key = match split_uri(&device.uri) {
            Some((BUILTIN_SCHEME, key)) => key,
            _ => return Err(unknown()),
        };
        let builtin = BUILTINS.iter().find(|b| b.key == key).ok_or_else(unknown)?;

        let mut processor = (builtin.create)();
        processor.prepare(sample_rate, max_block);
        for (index, def) in builtin.params.iter().enumerate() {
            if let Some(value) = device.params.get(def.id) {
                processor.set_param(index, *value);
            }
        }
        Ok(DeviceInstance { processor, params: builtin.params.iter().map(ParamDef::info).collect(), controller: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_core::device::{Event, EventKind, ProcessInfo};
    use daw_core::{ChainRef, Project};

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

    fn make(key: &str, params: &[(&str, f64)]) -> Box<dyn DeviceProcessor> {
        let mut project = Project::new("t");
        let id = project.add_device(ChainRef::Master, format!("builtin:{key}"), key, None).unwrap();
        let device = project.device_mut(id).unwrap();
        for (name, value) in params {
            device.params.insert(name.to_string(), *value);
        }
        BuiltinFactory.create(device, SR, BLOCK).unwrap().processor
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
        assert!(matches!(BuiltinFactory.create(project.device(id).unwrap(), SR, BLOCK), Err(DeviceError::Unknown(_))));
    }
}
