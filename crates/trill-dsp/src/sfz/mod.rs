//! Reading an instrument from an SFZ file.
//!
//! SFZ is a text file that says which recording plays for which keys and
//! how hard they are struck, with a few hundred settings ("opcodes") for
//! everything a player might do to it. [`read`] turns one into the model
//! the player takes ([`crate::sampled::Instrument`]). It knows nothing of
//! playing; another format is another reader beside this one.
//!
//! # What is read
//!
//! * The file's shape: `<control>`, `<global>`, `<master>`, `<group>`,
//!   `<region>`; `#include` and `#define`; `default_path`, `note_offset`,
//!   `octave_offset`, `set_ccN`, `label_ccN`, `label_keyN`; keys as numbers
//!   or names (`c4` is 60).
//! * Where a recording sounds: `sample`, `lokey`, `hikey`, `key`,
//!   `pitch_keycenter`, `lovel`, `hivel`, `loccN`, `hiccN`, `lorand`,
//!   `hirand`, `seq_length`, `seq_position`, `trigger` (`attack`,
//!   `release`, `release_key`, `first`, `legato`), `rt_decay`, `sw_lokey`,
//!   `sw_hikey`, `sw_last`, `sw_lolast`, `sw_hilast`, `sw_default`,
//!   `sw_label`, `sw_down`, `sw_up`, `sw_previous`, `group`, `off_by`,
//!   `off_mode`, `off_time`, `note_polyphony`, `sustain_sw`.
//! * Its pitch: `tune`, `transpose`, `pitch_keytrack`, `pitch_veltrack`,
//!   `pitch_random`, `bend_up`, `bend_down`, `tune_onccN`.
//! * Its level and place: `volume`, `amplitude`, `pan`, `width`,
//!   `amp_veltrack`, `amp_velcurve_N`, `amp_keytrack`, `amp_keycenter`,
//!   `amp_random`, `volume_onccN` (`gain_ccN`), `amplitude_onccN`,
//!   `pan_onccN`, and the crossfades `xfin_`/`xfout_` over `lokey`/`hikey`,
//!   `lovel`/`hivel` and `loccN`/`hiccN` with `xf_keycurve`, `xf_velcurve`,
//!   `xf_cccurve`.
//! * How it is played through: `offset`, `offset_random`, `end`, `delay`,
//!   `delay_random`, `loop_mode`, `loop_start`, `loop_end`,
//!   `loop_crossfade` (and the loop a WAV file carries itself).
//! * Its envelope: `ampeg_delay`, `ampeg_start`, `ampeg_attack`,
//!   `ampeg_hold`, `ampeg_decay`, `ampeg_sustain`, `ampeg_release`, each
//!   with `ampeg_vel2…` and `ampeg_…_onccN`.
//! * One filter: `fil_type` (`lpf_1p`, `hpf_1p`, `lpf_2p`, `hpf_2p`,
//!   `bpf_2p`, `lpf_4p`), `cutoff`, `resonance`, `fil_veltrack`,
//!   `fil_keytrack`, `fil_keycenter`, `cutoff_onccN`, and its envelope
//!   `fileg_…` with `fileg_depth`.
//! * Vibrato and tremolo: `pitchlfo_freq`, `pitchlfo_depth`,
//!   `pitchlfo_delay`, `pitchlfo_fade`, `pitchlfo_depth_onccN`, and the
//!   same of `amplfo_`.
//!
//! Everything else is counted, by name, with what a listener misses for
//! it ([`crate::sampled::Unsupported`]): the instrument still plays.

mod text;

use crate::sampled::model::{
    Control, Envelope, Fade, FadeCurve, FilterKind, Instrument, LoopMode, Moved, OffMode, Region, Stage, Trigger,
    Unsupported,
};
use std::collections::BTreeMap;
use std::path::Path;
use text::Token;

/// The extension of the files this reads.
pub const EXTENSION: &str = "sfz";

/// Whether `path` is named as an SFZ file is.
pub fn is_sfz(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case(EXTENSION))
}

/// Reads the instrument in the SFZ file at `path`, taking the text of it
/// and of the files it includes from `source`. An error is the end of a
/// sentence about the file.
pub fn read(path: &Path, source: &dyn Fn(&Path) -> Result<String, String>) -> Result<Instrument, String> {
    let text = text::tokens(path, source)?;
    let mut reader = Reader::default();
    reader.instrument.name = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    reader.instrument.pedal = true;
    reader.instrument.problems = text.problems;
    for token in text.tokens {
        match token {
            Token::Header(name) => reader.header(&name),
            Token::Opcode(name, value) => reader.opcode(name, value),
        }
    }
    reader.finish_region();
    let Reader { mut instrument, unsupported, labels, .. } = reader;
    if instrument.regions.is_empty() {
        return Err("it has no regions that play a recording".to_string());
    }
    // What a switch chooses is said beside the regions it chooses.
    for (key, label) in labels {
        instrument.switches.labels.entry(key).or_insert(label);
    }
    if instrument.switches.keys.is_none() {
        // An instrument that names its switches and not where they lie.
        let keys: Vec<u8> =
            instrument.regions.iter().filter_map(|region| region.switched.last).flat_map(|(a, b)| [a, b]).collect();
        if let (Some(low), Some(high)) = (keys.iter().min(), keys.iter().max()) {
            instrument.switches.keys = Some((*low, *high));
        }
    }
    instrument.unsupported =
        unsupported.into_iter().map(|(name, count)| Unsupported { miss: missed(&name), name, count }).collect();
    Ok(instrument)
}

/// Which header the pairs that follow belong to.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
enum Level {
    #[default]
    None,
    Control,
    Global,
    Master,
    Group,
    Region,
    /// A header this reader does nothing with.
    Other,
}

type Pairs = Vec<(String, String)>;

#[derive(Default)]
struct Reader {
    instrument: Instrument,
    level: Level,
    global: Pairs,
    master: Pairs,
    group: Pairs,
    region: Option<Pairs>,
    /// What `<control>` said, for the regions after it.
    default_path: String,
    key_offset: i32,
    /// By name, how often.
    unsupported: BTreeMap<String, usize>,
    /// What each switch chooses.
    labels: BTreeMap<u8, String>,
    recordings: BTreeMap<String, u32>,
}

impl Reader {
    fn header(&mut self, name: &str) {
        self.finish_region();
        self.level = match name {
            "control" => Level::Control,
            "global" => {
                self.global.clear();
                self.master.clear();
                self.group.clear();
                Level::Global
            }
            "master" => {
                self.master.clear();
                self.group.clear();
                Level::Master
            }
            "group" => {
                self.group.clear();
                Level::Group
            }
            "region" => {
                self.region = Some(Vec::new());
                Level::Region
            }
            other => {
                *self.unsupported.entry(format!("<{other}>")).or_default() += 1;
                Level::Other
            }
        };
    }

    fn opcode(&mut self, name: String, value: String) {
        match self.level {
            Level::Control => self.control(&name, &value),
            Level::Global => self.global.push((name, value)),
            Level::Master => self.master.push((name, value)),
            Level::Group => self.group.push((name, value)),
            Level::Region => self.region.get_or_insert_default().push((name, value)),
            // Pairs before any header belong to every region.
            Level::None => self.global.push((name, value)),
            Level::Other => {}
        }
    }

    fn control(&mut self, name: &str, value: &str) {
        let (stem, number) = split_number(name);
        match (stem, number) {
            ("default_path", None) => self.default_path = value.replace('\\', "/"),
            ("note_offset", None) => self.key_offset += integer(value).unwrap_or(0),
            ("octave_offset", None) => self.key_offset += 12 * integer(value).unwrap_or(0),
            ("set_cc", Some(controller)) if controller < 128 => {
                let position = (number_of(value).unwrap_or(0.0) / 127.0).clamp(0.0, 1.0);
                self.instrument.controller_starts.push((controller as u8, position));
            }
            ("set_hdcc", Some(controller)) if controller < 128 => {
                let position = number_of(value).unwrap_or(0.0).clamp(0.0, 1.0);
                self.instrument.controller_starts.push((controller as u8, position));
            }
            ("label_cc", Some(controller)) if controller < 128 => {
                self.instrument.controller_labels.insert(controller as u8, value.to_string());
            }
            ("label_key", Some(key)) => {
                if let Some(key) = self.offset_key(key as i32) {
                    self.instrument.key_labels.insert(key, value.to_string());
                }
            }
            _ if name.starts_with("hint_") || name.starts_with("label_") => {}
            _ => *self.unsupported.entry(name.to_string()).or_default() += 1,
        }
    }

    fn offset_key(&self, key: i32) -> Option<u8> {
        let key = key + self.key_offset;
        (0..=127).contains(&key).then_some(key as u8)
    }

    /// A key as a number or a name, moved by what `<control>` said.
    fn key(&self, value: &str) -> Option<u8> {
        self.offset_key(key_number(value)?)
    }

    fn finish_region(&mut self) {
        let Some(pairs) = self.region.take() else { return };
        let mut building = Building::default();
        // The levels are taken out while they are read, since reading one
        // may note something about the instrument as a whole.
        let levels =
            [std::mem::take(&mut self.global), std::mem::take(&mut self.master), std::mem::take(&mut self.group)];
        for (name, value) in levels.iter().flatten().chain(&pairs) {
            if !self.apply(&mut building, name, value) {
                *self.unsupported.entry(name.clone()).or_default() += 1;
            }
        }
        let [global, master, group] = levels;
        (self.global, self.master, self.group) = (global, master, group);
        self.keep(building);
    }

    /// Puts a finished region among the instrument's, if it plays.
    fn keep(&mut self, building: Building) {
        let Building { mut region, sample, label, curve, disabled, filter_envelope, filter_depth, tune, transpose } =
            building;
        region.tune = tune + 100.0 * transpose;
        if let (Some(label), Some((key, _))) = (label, region.switched.last) {
            self.labels.entry(key).or_insert(label);
        }
        let Some(sample) = sample else { return };
        if disabled {
            return;
        }
        region.recording = match sample.as_str() {
            "*silence" => None,
            generated if generated.starts_with('*') => {
                *self.unsupported.entry(format!("sample={generated}")).or_default() += 1;
                return;
            }
            name => {
                let whole = normalized(&format!("{}{}", self.default_path, name.replace('\\', "/")));
                let next = self.recordings.len() as u32;
                let index = *self.recordings.entry(whole.clone()).or_insert(next);
                if index == next {
                    self.instrument.recordings.push(whole);
                }
                Some(index)
            }
        };
        if !curve.is_empty() {
            region.velocity_curve = Some(velocity_curve(&curve));
        }
        if let Some(filter) = &mut region.filter
            && filter_depth != 0.0
        {
            filter.envelope = Some((filter_envelope, filter_depth));
        }
        if region.loop_mode.is_none() && (region.loop_start.is_some() || region.loop_end.is_some()) {
            region.loop_mode = Some(LoopMode::Continuous);
        }
        // A wave that moves nothing and that no controller deepens is none.
        let deepened = |moved: Moved| region.controls.iter().any(|control| control.moved == moved);
        if region.vibrato.is_some_and(|wave| wave.rate <= 0.0 || (wave.depth == 0.0 && !deepened(Moved::Vibrato))) {
            region.vibrato = None;
        }
        if region.tremolo.is_some_and(|wave| wave.rate <= 0.0 || (wave.depth == 0.0 && !deepened(Moved::Tremolo))) {
            region.tremolo = None;
        }
        if region.keys.0 > region.keys.1 || region.velocities.0 > region.velocities.1 {
            return;
        }
        self.instrument.regions.push(region);
    }

    /// Takes one pair into a region. False for one this reader does not do.
    fn apply(&mut self, building: &mut Building, name: &str, value: &str) -> bool {
        let region = &mut building.region;
        let number = number_of(value);
        let amount = number.unwrap_or(0.0);
        let key = self.key(value);
        let whole = |value: f32| value.max(0.0).round() as u32;
        match name {
            "sample" => building.sample = Some(value.to_string()),
            "lokey" => region.keys.0 = key.unwrap_or(0),
            "hikey" => region.keys.1 = key.unwrap_or(127),
            "key" => match key {
                Some(key) => {
                    region.keys = (key, key);
                    region.root = key;
                }
                None => building.disabled = true,
            },
            "pitch_keycenter" => match key {
                Some(key) => region.root = key,
                None => return false,
            },
            "lovel" => region.velocities.0 = amount.clamp(0.0, 127.0) as u8,
            "hivel" => region.velocities.1 = amount.clamp(0.0, 127.0) as u8,
            "lorand" => region.random.0 = amount,
            "hirand" => region.random.1 = amount,
            "seq_length" => region.turn.0 = whole(amount).clamp(1, u32::from(u16::MAX)) as u16,
            "seq_position" => region.turn.1 = whole(amount).clamp(1, u32::from(u16::MAX)) as u16,
            "sw_lokey" | "sw_hikey" => {
                let Some(key) = key else { return true };
                let (low, high) = self.instrument.switches.keys.unwrap_or((key, key));
                self.instrument.switches.keys =
                    Some(if name == "sw_lokey" { (key, high.max(key)) } else { (low.min(key), key) });
            }
            "sw_last" => region.switched.last = key.map(|key| (key, key)),
            "sw_lolast" => region.switched.last = key.map(|key| (key, region.switched.last.map_or(key, |(_, b)| b))),
            "sw_hilast" => region.switched.last = key.map(|key| (region.switched.last.map_or(key, |(a, _)| a), key)),
            "sw_default" => self.instrument.switches.default = key,
            "sw_label" => building.label = Some(value.to_string()),
            "sw_down" => region.switched.down = key,
            "sw_up" => region.switched.up = key,
            "sw_previous" => region.switched.previous = key,
            "trigger" => {
                region.trigger = match value.trim().to_lowercase().as_str() {
                    "attack" => Trigger::Attack,
                    "release" => Trigger::Release,
                    "release_key" => Trigger::ReleaseKey,
                    "first" => Trigger::First,
                    "legato" => Trigger::Legato,
                    _ => return false,
                }
            }
            "rt_decay" => region.release_decay = amount,
            "group" | "polyphony_group" => region.group = whole(amount),
            "off_by" | "offby" => region.off_by = Some(whole(amount)).filter(|group| *group != 0),
            "off_mode" | "offmode" => {
                region.off_mode = match (value.trim().to_lowercase().as_str(), region.off_mode) {
                    ("fast", _) => OffMode::Fast,
                    ("normal", _) => OffMode::Normal,
                    ("time", OffMode::Time(seconds)) => OffMode::Time(seconds),
                    ("time", _) => OffMode::Time(0.006),
                    _ => return false,
                }
            }
            "off_time" | "offtime" => region.off_mode = OffMode::Time(amount.max(0.0)),
            "note_polyphony" => region.note_polyphony = Some(whole(amount).clamp(1, 64) as u16),
            "sustain_sw" => self.instrument.pedal = !value.trim().eq_ignore_ascii_case("off"),

            "tune" | "pitch" => building.tune = amount,
            "transpose" => building.transpose = amount,
            "pitch_keytrack" => region.pitch_by_key = amount,
            "pitch_veltrack" => region.pitch_by_velocity = amount,
            "pitch_random" => region.pitch_random = amount.abs(),
            "bend_up" | "bendup" => region.bend_up = amount,
            "bend_down" | "benddown" => region.bend_down = amount,

            "volume" | "gain" => region.volume = amount,
            "amplitude" => region.amplitude = (amount / 100.0).max(0.0),
            "pan" => region.pan = (amount / 100.0).clamp(-1.0, 1.0),
            "width" => region.width = (amount / 100.0).clamp(-1.0, 1.0),
            "amp_veltrack" => region.level_by_velocity = (amount / 100.0).clamp(-1.0, 1.0),
            "amp_keytrack" => region.level_by_key = amount,
            "amp_keycenter" => region.level_centre = key.unwrap_or(60),
            "amp_random" => region.level_random = amount.abs(),

            "offset" => region.offset = whole(amount),
            "offset_random" => region.offset_random = whole(amount),
            "end" => match number {
                Some(end) if end > 0.0 => region.end = Some(whole(end)),
                Some(_) => building.disabled = true,
                None => return false,
            },
            "delay" => region.delay = amount.max(0.0),
            "delay_random" => region.delay_random = amount.max(0.0),
            "loop_mode" | "loopmode" => {
                region.loop_mode = Some(match value.trim().to_lowercase().as_str() {
                    "no_loop" => LoopMode::NoLoop,
                    "one_shot" => LoopMode::OneShot,
                    "loop_continuous" => LoopMode::Continuous,
                    "loop_sustain" => LoopMode::Sustain,
                    _ => return false,
                })
            }
            "loop_start" | "loopstart" => region.loop_start = Some(whole(amount)),
            "loop_end" | "loopend" => region.loop_end = Some(whole(amount)),
            "loop_crossfade" => region.loop_crossfade = amount.max(0.0),

            "fil_type" | "filtype" => {
                region.filter.get_or_insert_default().kind = match value.trim().to_lowercase().as_str() {
                    "lpf_1p" => FilterKind::LowPass1,
                    "hpf_1p" => FilterKind::HighPass1,
                    "lpf_2p" => FilterKind::LowPass2,
                    "hpf_2p" => FilterKind::HighPass2,
                    "bpf_2p" => FilterKind::BandPass2,
                    "lpf_4p" => FilterKind::LowPass4,
                    _ => return false,
                }
            }
            "cutoff" => region.filter.get_or_insert_default().cutoff = amount.clamp(10.0, 22_000.0),
            "resonance" => region.filter.get_or_insert_default().resonance = amount.clamp(0.0, 40.0),
            "fil_veltrack" => region.filter.get_or_insert_default().by_velocity = amount,
            "fil_keytrack" => region.filter.get_or_insert_default().by_key = amount,
            "fil_keycenter" => region.filter.get_or_insert_default().centre = key.unwrap_or(60),
            "fileg_depth" => building.filter_depth = amount,

            "xfin_lokey" => region.fade_keys.in_low = f32::from(key.unwrap_or(0)),
            "xfin_hikey" => region.fade_keys.in_high = f32::from(key.unwrap_or(0)),
            "xfout_lokey" => region.fade_keys.out_low = f32::from(key.unwrap_or(127)),
            "xfout_hikey" => region.fade_keys.out_high = f32::from(key.unwrap_or(127)),
            "xfin_lovel" => region.fade_velocities.in_low = amount,
            "xfin_hivel" => region.fade_velocities.in_high = amount,
            "xfout_lovel" => region.fade_velocities.out_low = amount,
            "xfout_hivel" => region.fade_velocities.out_high = amount,
            "xf_keycurve" | "xf_velcurve" | "xf_cccurve" => {
                let curve = match value.trim().to_lowercase().as_str() {
                    "gain" => FadeCurve::Gain,
                    "power" => FadeCurve::Power,
                    _ => return false,
                };
                match name {
                    "xf_keycurve" => region.key_curve = curve,
                    "xf_velcurve" => region.velocity_fade_curve = curve,
                    _ => region.controller_curve = curve,
                }
            }

            "pitchlfo_freq" => region.vibrato.get_or_insert_default().rate = amount,
            "pitchlfo_depth" => region.vibrato.get_or_insert_default().depth = amount,
            "pitchlfo_delay" => region.vibrato.get_or_insert_default().delay = amount.max(0.0),
            "pitchlfo_fade" => region.vibrato.get_or_insert_default().fade = amount.max(0.0),
            "amplfo_freq" => region.tremolo.get_or_insert_default().rate = amount,
            "amplfo_depth" => region.tremolo.get_or_insert_default().depth = amount,
            "amplfo_delay" => region.tremolo.get_or_insert_default().delay = amount.max(0.0),
            "amplfo_fade" => region.tremolo.get_or_insert_default().fade = amount.max(0.0),

            // What says nothing about the sound here: which MIDI channel,
            // which output, how carefully to interpolate, names for a
            // maker's own editor.
            "lochan" | "hichan" | "output" | "sample_quality" | "region_label" | "group_label" | "master_label"
            | "global_label" | "script" | "md5" => {}
            _ if name.starts_with("hint_") => {}
            _ => return self.numbered(building, name, amount, value),
        }
        true
    }

    /// The pairs whose name ends in a number: a controller's, or a point
    /// of a curve.
    fn numbered(&mut self, building: &mut Building, name: &str, amount: f32, value: &str) -> bool {
        // The envelopes, with and without a controller.
        for (prefix, filter) in [("ampeg_", false), ("fileg_", true)] {
            let Some(rest) = name.strip_prefix(prefix) else { continue };
            let envelope = if filter { &mut building.filter_envelope } else { &mut building.region.envelope };
            return envelope_pair(envelope, rest, amount);
        }
        let (stem, number) = split_number(name);
        let Some(number) = number else { return false };
        let region = &mut building.region;
        if stem == "amp_velcurve_" {
            if number <= 127 {
                building.curve.push((number as u8, amount.clamp(0.0, 1.0)));
            }
            return true;
        }
        if number > 127 {
            // Controllers above 127 are a player's own: the pitch wheel,
            // the pressure, a random number.
            return false;
        }
        let controller = number as u8;
        let stem = canonical(stem);
        let fade = |region: &mut Region, set: &dyn Fn(&mut Fade)| {
            if !region.fade_controllers.iter().any(|(number, _)| *number == controller) {
                region.fade_controllers.push((controller, Fade::default()));
            }
            let found = region.fade_controllers.iter_mut().find(|(number, _)| *number == controller);
            set(&mut found.expect("it was just put there").1);
        };
        let moved = match stem.as_str() {
            "locc" | "hicc" => {
                if !region.when.iter().any(|(number, _, _)| *number == controller) {
                    region.when.push((controller, 0.0, 127.0));
                }
                let span = region.when.iter_mut().find(|(number, _, _)| *number == controller).expect("just put");
                if stem == "locc" {
                    span.1 = amount
                } else {
                    span.2 = amount
                }
                return true;
            }
            "xfin_locc" => {
                return {
                    fade(region, &|fade| fade.in_low = amount);
                    true
                };
            }
            "xfin_hicc" => {
                return {
                    fade(region, &|fade| fade.in_high = amount);
                    true
                };
            }
            "xfout_locc" => {
                return {
                    fade(region, &|fade| fade.out_low = amount);
                    true
                };
            }
            "xfout_hicc" => {
                return {
                    fade(region, &|fade| fade.out_high = amount);
                    true
                };
            }
            "volume_oncc" | "gain_oncc" => Moved::Volume,
            "amplitude_oncc" => Moved::Amplitude,
            "pan_oncc" => Moved::Pan,
            "tune_oncc" | "pitch_oncc" => Moved::Tune,
            "cutoff_oncc" => Moved::Cutoff,
            "pitchlfo_depth_oncc" => Moved::Vibrato,
            "amplfo_depth_oncc" => Moved::Tremolo,
            _ => return false,
        };
        let _ = value;
        match moved {
            Moved::Vibrato => drop(region.vibrato.get_or_insert_default()),
            Moved::Tremolo => drop(region.tremolo.get_or_insert_default()),
            Moved::Cutoff => drop(region.filter.get_or_insert_default()),
            _ => {}
        }
        region.controls.retain(|control| !(control.controller == controller && control.moved == moved));
        region.controls.push(Control { controller, moved, depth: amount });
        true
    }
}

/// A region as its pairs come in.
#[derive(Default)]
struct Building {
    region: Region,
    sample: Option<String>,
    /// What the region's switch chooses.
    label: Option<String>,
    /// Points of its velocity curve.
    curve: Vec<(u8, f32)>,
    /// It says of itself that it does not play.
    disabled: bool,
    tune: f32,
    transpose: f32,
    filter_envelope: Envelope,
    filter_depth: f32,
}

/// One pair of an envelope: `attack`, `vel2attack`, `attack_oncc72`.
fn envelope_pair(envelope: &mut Envelope, rest: &str, amount: f32) -> bool {
    let stage_of = |word: &str| {
        Some(match word {
            "delay" => Stage::Delay,
            "attack" => Stage::Attack,
            "hold" => Stage::Hold,
            "decay" => Stage::Decay,
            "sustain" => Stage::Sustain,
            "release" => Stage::Release,
            _ => return None,
        })
    };
    // Levels are written in percent.
    let scaled = |stage: Stage| if stage == Stage::Sustain { amount / 100.0 } else { amount };
    if let Some(stage) = stage_of(rest) {
        match stage {
            Stage::Delay => envelope.delay = amount.max(0.0),
            Stage::Attack => envelope.attack = amount.max(0.0),
            Stage::Hold => envelope.hold = amount.max(0.0),
            Stage::Decay => envelope.decay = amount.max(0.0),
            Stage::Sustain => envelope.sustain = (amount / 100.0).clamp(0.0, 1.0),
            Stage::Release => envelope.release = amount.max(0.0),
        }
        return true;
    }
    if rest == "start" {
        envelope.start = (amount / 100.0).clamp(0.0, 1.0);
        return true;
    }
    if let Some(stage) = rest.strip_prefix("vel2").and_then(stage_of) {
        envelope.by_velocity.retain(|(which, _)| *which != stage);
        envelope.by_velocity.push((stage, scaled(stage)));
        return true;
    }
    let (stem, number) = split_number(rest);
    let word = stem.strip_suffix("_oncc").or_else(|| stem.strip_suffix("cc"));
    match (word.and_then(stage_of), number) {
        (Some(stage), Some(controller)) if controller <= 127 => {
            let controller = controller as u8;
            envelope.by_controller.retain(|(which, number, _)| !(*which == stage && *number == controller));
            envelope.by_controller.push((stage, controller, scaled(stage)));
            true
        }
        _ => false,
    }
}

/// A name without the number it ends in, and the number.
fn split_number(name: &str) -> (&str, Option<u32>) {
    let digits = name.bytes().rev().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits == name.len() {
        return (name, None);
    }
    let (stem, number) = name.split_at(name.len() - digits);
    (stem, number.parse().ok())
}

/// The one name a controller's pair goes by: `gain_cc7` and
/// `volume_oncc7` are the same thing said in two versions of the format.
fn canonical(stem: &str) -> String {
    const AS_WRITTEN: &[&str] = &["locc", "hicc", "xfin_locc", "xfin_hicc", "xfout_locc", "xfout_hicc"];
    if AS_WRITTEN.contains(&stem) || stem.ends_with("_oncc") {
        return stem.to_string();
    }
    match stem.strip_suffix("_cc").or_else(|| stem.strip_suffix("cc")) {
        Some(word) => format!("{word}_oncc"),
        None => stem.to_string(),
    }
}

/// The number a value begins with: `-12.5`, `100`, `3 // three`.
fn number_of(value: &str) -> Option<f32> {
    let value = value.trim();
    let end = value
        .char_indices()
        .find(|(index, c)| !(c.is_ascii_digit() || *c == '.' || (*index == 0 && (*c == '-' || *c == '+'))))
        .map_or(value.len(), |(index, _)| index);
    value[..end].parse::<f32>().ok().filter(|number| number.is_finite())
}

fn integer(value: &str) -> Option<i32> {
    number_of(value).map(|number| number.round() as i32)
}

/// A key as a number, or as a name: `c4` is 60, `c#4` 61, `db4` 61, `a-1` 9.
pub(crate) fn key_number(value: &str) -> Option<i32> {
    let value = value.trim().to_lowercase();
    if let Some(number) = number_of(&value) {
        return Some(number.round() as i32);
    }
    let mut chars = value.chars();
    let letter = match chars.next()? {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => return None,
    };
    let rest = chars.as_str();
    let (accidental, octave) = match rest.chars().next()? {
        '#' => (1, &rest[1..]),
        'b' => (-1, &rest[1..]),
        _ => (0, rest),
    };
    let octave: i32 = octave.parse().ok()?;
    Some(letter + accidental + 12 * (octave + 1))
}

/// A path as the instrument writes it with its `.` and `..` parts taken
/// out, as far as they can be.
fn normalized(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "." => {}
            "" if !parts.is_empty() => {}
            ".." if parts.last().is_some_and(|last| *last != ".." && !last.is_empty() && !last.ends_with(':')) => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

/// The gain for each velocity of a curve given by `points`: a straight
/// line from one point to the next, from nothing at velocity 0 to full at
/// 127 where the points do not say.
fn velocity_curve(points: &[(u8, f32)]) -> Box<[f32; 128]> {
    let mut points: Vec<(u8, f32)> = points.to_vec();
    if !points.iter().any(|(velocity, _)| *velocity == 0) {
        points.push((0, 0.0));
    }
    if !points.iter().any(|(velocity, _)| *velocity == 127) {
        points.push((127, 1.0));
    }
    points.sort_by_key(|(velocity, _)| *velocity);
    let mut curve = Box::new([0.0f32; 128]);
    for pair in points.windows(2) {
        let ((from, low), (to, high)) = (pair[0], pair[1]);
        for velocity in from..=to {
            let along = if to > from { f32::from(velocity - from) / f32::from(to - from) } else { 0.0 };
            curve[usize::from(velocity)] = low + (high - low) * along;
        }
    }
    curve
}

/// What a listener misses for something this player does not do.
fn missed(name: &str) -> &'static str {
    let name = name.trim_start_matches('<');
    let starts = |words: &[&str]| words.iter().any(|word| name.starts_with(word));
    if starts(&["effect", "reverb", "chorus", "fx", "apan", "comp_", "gate_", "disto", "strings_"]) {
        "effects of its own (a reverb, a chorus)"
    } else if starts(&["eq"]) {
        "equalisers of its own"
    } else if starts(&["lfo", "eg", "pitcheg", "fillfo", "amplfo", "pitchlfo", "ampeg", "fileg"]) {
        "movement its maker gave the pitch, the level or the tone beyond a vibrato and a tremolo"
    } else if starts(&["cutoff2", "fil2", "resonance2", "noise", "oscillator", "sample=*", "sample_dyn", "waveguide"]) {
        "sound that is made and not recorded (a second filter, generated waves)"
    } else if starts(&["on_locc", "on_hicc", "start_locc", "start_hicc", "stop_"]) {
        "sounds that moving a controller sets off (the noise of a pedal)"
    } else if starts(&["curve"]) || name.contains("_curvecc") {
        "controller curves of its own (a controller acts along a straight line)"
    } else if starts(&["direction", "count", "loop_count", "sync", "delay_", "offset_", "loop_", "sample_fade"]) {
        "ways of playing a recording through (backwards, a number of times, in time with the song)"
    } else if starts(&["sostenuto", "sustain_", "polyphony", "note_selfmask", "rt_dead"]) {
        "its rules for pedals and for how many voices sound at once"
    } else if starts(&["lobend", "hibend", "lochanaft", "hichanaft", "lopolyaft", "hipolyaft", "lobpm", "hibpm"])
        || starts(&["lotimer", "hitimer", "sw_vel", "lohdcc", "hihdcc", "loprog", "hiprog", "pitch_keycenter"])
    {
        "conditions on which recording sounds (the pitch wheel, the pressure, the tempo, a file's own root note)"
    } else if name.contains("_smoothcc") || name.contains("_stepcc") || starts(&["bend_step", "bend_smooth"]) {
        "how smoothly or in what steps a controller moves things"
    } else {
        "settings this player does not know"
    }
}

/// Reads a region's pairs with the tuning put together at the end; the
/// reader's own entry point, apart for tests.
impl Reader {
    #[cfg(test)]
    fn region_of(&mut self, pairs: &[(&str, &str)]) -> Option<Region> {
        self.region = Some(pairs.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect());
        let before = self.instrument.regions.len();
        self.finish_region();
        (self.instrument.regions.len() > before).then(|| self.instrument.regions.last().cloned()).flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampled::model::Wobble;

    fn read_text(source: &'static str) -> Result<Instrument, String> {
        read(Path::new("/kits/Small Grand.sfz"), &|path| {
            if path == Path::new("/kits/Small Grand.sfz") { Ok(source.to_string()) } else { Err("no such file".into()) }
        })
    }

    #[test]
    fn keys_are_numbers_or_names() {
        for (text, key) in [("60", 60), ("c4", 60), ("C4", 60), ("c#4", 61), ("db4", 61), ("a-1", 9), ("g9", 127)] {
            assert_eq!(key_number(text), Some(key), "{text}");
        }
        assert_eq!(key_number("b#3"), Some(60));
        assert_eq!(key_number("-1"), Some(-1));
        for nonsense in ["", "h4", "c", "sample", "c#"] {
            assert_eq!(key_number(nonsense), None, "{nonsense}");
        }
    }

    #[test]
    fn a_region_takes_what_its_group_and_everything_above_it_says() {
        let piano = read_text(
            "<control> default_path=Samples\\ octave_offset=1 note_offset=-12 label_cc64=Sustain set_cc7=100\n\
             <global> ampeg_release=0.8 amp_veltrack=90\n\
             <group> lovel=1 hivel=60 volume=-3\n\
             <region> sample=C2 soft.wav lokey=35 hikey=37 pitch_keycenter=c2\n\
             <region> sample=./E2 soft.wav key=e2 tune=-12 transpose=1 volume=2\n\
             <group> lovel=61 hivel=127\n\
             <region> sample=C2 loud.wav lokey=35 hikey=37 pitch_keycenter=36 pan=-30\n\
             <master> amp_veltrack=50 <region> sample=../Shared/noise.wav trigger=release rt_decay=6",
        )
        .unwrap();
        assert_eq!(piano.name, "Small Grand");
        assert_eq!(
            piano.recordings,
            ["Samples/C2 soft.wav", "Samples/E2 soft.wav", "Samples/C2 loud.wav", "Shared/noise.wav"]
        );
        let [soft, e, loud, noise] = piano.regions.as_slice() else { panic!("{} regions", piano.regions.len()) };
        assert_eq!((soft.keys, soft.root, soft.velocities), ((35, 37), 36, (1, 60)));
        assert_eq!((soft.volume, soft.envelope.release, soft.level_by_velocity), (-3.0, 0.8, 0.9));
        assert_eq!(
            (e.keys, e.root, e.tune, e.volume),
            ((40, 40), 40, 88.0, 2.0),
            "its own volume in place of the group's"
        );
        assert_eq!((loud.velocities, loud.volume, loud.pan), ((61, 127), 0.0, -0.3), "a new group starts afresh");
        assert_eq!((noise.trigger, noise.release_decay, noise.level_by_velocity), (Trigger::Release, 6.0, 0.5));
        assert_eq!(noise.velocities, (0, 127), "a new master starts the groups afresh too");
        assert_eq!(noise.envelope.release, 0.8, "and what is global stays");
        assert_eq!(piano.controller_labels.get(&64).map(String::as_str), Some("Sustain"));
        assert_eq!(piano.controller_starts, [(7, 100.0 / 127.0)]);
        assert!(piano.pedal && piano.unsupported.is_empty() && piano.problems.is_empty());
    }

    #[test]
    fn takes_in_turn_and_at_random_layers_that_fade_and_switches_are_read() {
        let strings = read_text(
            "<global> sw_lokey=c1 sw_hikey=d1 sw_default=c1\n\
             <group> sw_last=c1 sw_label=Sustain loop_mode=loop_sustain loop_start=1000 loop_end=9000 loop_crossfade=0.05\n\
             <region> sample=s_p.wav xfin_locc1=0 xfin_hicc1=0 xfout_locc1=40 xfout_hicc1=90 pitchlfo_freq=5 pitchlfo_depth_oncc1=30\n\
             <region> sample=s_f.wav xfin_locc1=40 xfin_hicc1=90 xf_cccurve=gain ampeg_attack_oncc73=2 ampeg_vel2attack=-0.1\n\
             <group> sw_last=d1 sw_label=Pizzicato loop_mode=one_shot\n\
             <region> sample=pz1.wav seq_length=2 seq_position=1 lorand=0 hirand=0.5\n\
             <region> sample=pz2.wav seq_length=2 seq_position=2 lorand=0.5 hirand=1 amp_velcurve_64=0.8\n\
             <region> sample=pz3.wav key=-1\n<region> sample=pz4.wav end=-1\n<region> lokey=10",
        )
        .unwrap();
        assert_eq!(strings.regions.len(), 4, "a region that says it does not play, or names no recording, is none");
        assert_eq!(strings.switches.keys, Some((24, 26)));
        assert_eq!(strings.switches.default, Some(24));
        assert_eq!(strings.switches.labels, [(24, "Sustain".to_string()), (26, "Pizzicato".to_string())].into());
        let [soft, loud, first, second] = strings.regions.as_slice() else { panic!() };
        assert_eq!(soft.switched.last, Some((24, 24)));
        assert_eq!(
            (soft.loop_mode, soft.loop_start, soft.loop_end),
            (Some(LoopMode::Sustain), Some(1_000), Some(9_000))
        );
        assert_eq!(soft.loop_crossfade, 0.05);
        assert_eq!(soft.fade_controllers, [(1, Fade { in_low: 0.0, in_high: 0.0, out_low: 40.0, out_high: 90.0 })]);
        assert_eq!(loud.fade_controllers, [(1, Fade { in_low: 40.0, in_high: 90.0, out_low: 127.0, out_high: 127.0 })]);
        assert_eq!((soft.controller_curve, loud.controller_curve), (FadeCurve::Power, FadeCurve::Gain));
        assert_eq!(soft.vibrato, Some(Wobble { rate: 5.0, ..Wobble::default() }));
        assert_eq!(soft.controls, [Control { controller: 1, moved: Moved::Vibrato, depth: 30.0 }]);
        assert_eq!(loud.envelope.by_controller, [(Stage::Attack, 73, 2.0)]);
        assert_eq!(loud.envelope.by_velocity, [(Stage::Attack, -0.1)]);
        assert_eq!((first.turn, first.random), ((2, 1), (0.0, 0.5)));
        assert_eq!((second.turn, second.random), ((2, 2), (0.5, 1.0)));
        assert_eq!(second.loop_mode, Some(LoopMode::OneShot));
        let curve = second.velocity_curve.as_ref().unwrap();
        assert!(
            (curve[64] - 0.8).abs() < 1e-6 && (curve[32] - 0.4).abs() < 1e-6 && curve[127] == 1.0 && curve[0] == 0.0
        );
        let summary = strings.summary();
        assert_eq!(summary.switches, [(24, "Sustain".to_string()), (26, "Pizzicato".to_string())]);
        assert_eq!(summary.controllers.iter().map(|(number, _)| *number).collect::<Vec<_>>(), [1, 73]);
    }

    #[test]
    fn what_this_player_does_not_do_is_counted_by_name_and_the_rest_still_reads() {
        let kit = read_text(
            "<control> hint_ram_based=1 set_cc300=1\n<global> eq1_gain=3 eq1_freq=80 lfo01_freq=4 lfo01_pitch=10\n\
             <effect> type=reverb\n<curve> curve_index=7 v000=0\n\
             <group> key=36 loop_mode=one_shot off_by=2 group=1 off_mode=time off_time=0.2 polyphony=4\n\
             <region> sample=kick.wav on_locc64=1 cutoff=900 fil_type=lpf_2p fil_veltrack=1200 fileg_depth=2400 fileg_decay=0.1\n\
             <region> sample=*sine key=40\n<region> sample=*silence key=41 group=2\n\
             <region> sample=snare.wav key=38 direction=reverse trigger=whenever gain_cc7=-6 amplitude_cc11=100",
        )
        .unwrap();
        let named: Vec<(&str, usize)> = kit.unsupported.iter().map(|un| (un.name.as_str(), un.count)).collect();
        for expected in [
            ("eq1_gain", 4),
            ("eq1_freq", 4),
            ("lfo01_freq", 4),
            ("<effect>", 1),
            ("<curve>", 1),
            ("polyphony", 4),
            ("on_locc64", 1),
            ("sample=*sine", 1),
            ("direction", 1),
            ("trigger", 1),
            ("set_cc300", 1),
        ] {
            assert!(named.contains(&expected), "{expected:?} is not among {named:?}");
        }
        assert!(!named.iter().any(|(name, _)| name.starts_with("hint") || *name == "cutoff" || *name == "type"));
        let effects = kit.unsupported.iter().find(|un| un.name == "<effect>").unwrap();
        assert!(effects.miss.contains("effects of its own"));
        assert!(kit.unsupported.iter().find(|un| un.name == "eq1_gain").unwrap().miss.contains("equalisers"));
        // And what it does do is all there.
        let [kick, silence, snare] = kit.regions.as_slice() else { panic!("{}", kit.regions.len()) };
        assert_eq!((kick.keys, kick.off_by, kick.group, kick.off_mode), ((36, 36), Some(2), 1, OffMode::Time(0.2)));
        let filter = kick.filter.as_ref().unwrap();
        assert_eq!((filter.kind, filter.cutoff, filter.by_velocity), (FilterKind::LowPass2, 900.0, 1_200.0));
        let (envelope, depth) = filter.envelope.as_ref().unwrap();
        assert_eq!((envelope.decay, *depth), (0.1, 2_400.0));
        assert_eq!((silence.recording, silence.group), (None, 2));
        assert_eq!(snare.controls.len(), 2);
        assert_eq!(snare.controls[0], Control { controller: 7, moved: Moved::Volume, depth: -6.0 });
        assert_eq!(snare.controls[1], Control { controller: 11, moved: Moved::Amplitude, depth: 100.0 });
        assert!(kit.summary().drums);

        assert_eq!(read_text("<group> lokey=1").unwrap_err(), "it has no regions that play a recording");
        assert_eq!(read(Path::new("/x.sfz"), &|_| Err("no such file".into())).unwrap_err(), "no such file");
    }

    #[test]
    fn a_region_can_be_built_from_pairs_alone() {
        let mut reader = Reader::default();
        let region = reader.region_of(&[("sample", "a.wav"), ("tune", "50"), ("transpose", "-1")]).unwrap();
        assert_eq!(region.tune, -50.0);
        assert!(reader.region_of(&[("lokey", "4")]).is_none());
    }
}
