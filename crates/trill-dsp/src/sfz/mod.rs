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
//!   `off_mode`, `off_time`, `polyphony`, `note_polyphony`, `sustain_sw`,
//!   `sustain_cc`.
//! * What no key sounds, and a controller sets off by coming into a span:
//!   `on_loccN`, `on_hiccN` (and `on_lohdccN`, `on_hihdccN`), with
//!   `lokey=-1` for "no key". The noise of a pedal going down and of its
//!   coming up.
//! * Its pitch: `tune`, `transpose`, `pitch_keytrack`, `pitch_veltrack`,
//!   `pitch_random`, `bend_up`, `bend_down`, `tune_onccN`.
//! * Its level and place: `volume` (and `global_volume`, `master_volume`,
//!   `group_volume`, which add up), `amplitude`, `pan`, `width`,
//!   `amp_veltrack`, `amp_velcurve_N`, `amp_keytrack`, `amp_keycenter`,
//!   `amp_random`, `volume_onccN` (`gain_ccN`), `amplitude_onccN`,
//!   `pan_onccN`, `width_onccN`, `amp_veltrack_onccN`, and the crossfades `xfin_`/`xfout_` over `lokey`/`hikey`,
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
//! * Up to six more slow waves, numbered: `lfoN_freq`, `lfoN_delay`,
//!   `lfoN_fade`, `lfoN_phase`, `lfoN_wave` (sine, triangle, square, the
//!   two saws, and a level drawn by lot each time round), and what each
//!   moves: `lfoN_pitch`, `lfoN_volume`, `lfoN_cutoff`, `lfoN_pan`, and
//!   the rate of another (`lfoN_freq_lfoM`). Every one of those may be
//!   moved by a controller (`…_onccN`, along `…_curveccN`).
//! * By a controller when a note begins: where in the recording it starts
//!   (`offset_onccN`) and how long it waits (`delay_onccN`); and while it
//!   sounds, how far the filter rings (`resonance_onccN`) and how far its
//!   envelope opens it (`fileg_depth_onccN`).
//! * The curves controllers are read along: `…_curveccN` on everything a
//!   controller moves here, with the seven curves every player has and
//!   the instrument's own (`<curve>` with `curve_index` and `vNNN`).
//!
//! Controllers are the 128 of MIDI, and five that a note has of its own,
//! by the numbers the format's makers gave them: how hard it was struck
//! (131), its key (133), a number at random (135, and 136 for one that
//! goes both ways) and one that is 0 and 1 by turns (137). Wherever a
//! controller may be named, one of those may be. The others a maker's
//! player makes up (how fast a key came up, how far a note is from the
//! one before) are not here.
//!
//! Numbers from 150 up are controllers that only a maker's own window
//! moves (the level of each microphone of a kit). Nothing moves them here,
//! so each stands where the file puts it (`set_ccN`, `set_hdccN`; at
//! nothing where it does not say), and what it does is worked out as the
//! file is read: whether a region sounds at all (`loccN`, `hiccN`), and
//! its level, place and tuning (`amplitude_onccN`, `volume_onccN`,
//! `pan_onccN`, `tune_onccN`).
//! Controller 130 is the pressure on one key after it went down. Nothing
//! in a song here carries that, so it stands at rest: a region that asks
//! for it to be up is left out (a cymbal that is choked by leaning on its
//! pad is choked by its choke key, or not), and one that it would set off
//! is never set off. Neither is counted: nothing a song could do is
//! missed. A region that no key plays (`key=-1`) and no controller sets
//! off is left out.
//!
//! Everything else is counted, by name, with what a listener misses for
//! it ([`crate::sampled::Unsupported`]): the instrument still plays.

mod text;

use crate::sampled::model::{
    BUILT_IN_CURVES, Control, Envelope, Fade, FadeCurve, FilterKind, Instrument, LoopMode, MAX_WAVES, Moved, OffMode,
    Region, SlowWave, Stage, Swing, Swung, Trigger, Unsupported, Wave, built_in_curves, note,
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
    reader.instrument.pedal_controller = 64;
    reader.instrument.curves = built_in_curves();
    reader.instrument.problems = text.problems;
    for token in text.tokens {
        match token {
            Token::Header(name) => reader.header(&name),
            Token::Opcode(name, value) => reader.opcode(name, value),
        }
    }
    reader.finish_region();
    reader.finish_curve();
    let Reader { mut instrument, unsupported, labels, curves, .. } = reader;
    if instrument.regions.is_empty() {
        return Err("it has no regions that play a recording".to_string());
    }
    // The instrument's own curves come after the ones every player has;
    // what asked for one by its number in the file is told where it is.
    let mut places: BTreeMap<u16, u16> = BTreeMap::new();
    for (number, table) in curves {
        if usize::from(number) >= BUILT_IN_CURVES {
            places.insert(number, instrument.curves.len() as u16);
            instrument.curves.push(table);
        }
    }
    let placed = |curve: &mut u16| {
        if usize::from(*curve) >= BUILT_IN_CURVES {
            *curve = places.get(curve).copied().unwrap_or(0);
        }
    };
    for region in &mut instrument.regions {
        region.controls.iter_mut().for_each(|control| placed(&mut control.curve));
        region.envelope.by_controller.iter_mut().for_each(|(_, _, _, curve)| placed(curve));
        if let Some((envelope, _)) = region.filter.as_mut().and_then(|filter| filter.envelope.as_mut()) {
            envelope.by_controller.iter_mut().for_each(|(_, _, _, curve)| placed(curve));
        }
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
    /// A curve of the instrument's own.
    Curve,
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
    /// The instrument's own curves, by their numbers in the file, and the
    /// one that is being read: its number and its points.
    curves: BTreeMap<u16, [f32; 128]>,
    curve: Option<CurveRead>,
    /// Where the file puts the controllers that nothing here moves
    /// ([`STANDING`]), 0..=1.
    standing: BTreeMap<u32, f32>,
}

/// The controllers that only a maker's own window moves, by their
/// numbers: they stand where the file puts them.
const STANDING: std::ops::Range<u32> = 150..1024;

/// A curve while its header is being read: its number, if it has said
/// one, and the points it has given so far.
type CurveRead = (Option<u16>, Vec<(u8, f32)>);

impl Reader {
    /// Keeps the curve that was being read: a straight line from each of
    /// its points to the next, from 0 at the bottom to 1 at the top where
    /// it does not say.
    fn finish_curve(&mut self) {
        let Some((Some(number), mut points)) = self.curve.take() else { return };
        if !points.iter().any(|(at, _)| *at == 0) {
            points.push((0, 0.0));
        }
        if !points.iter().any(|(at, _)| *at == 127) {
            points.push((127, 1.0));
        }
        points.sort_by_key(|(at, _)| *at);
        let mut table = [0.0f32; 128];
        for pair in points.windows(2) {
            let ((from, low), (to, high)) = (pair[0], pair[1]);
            for at in from..=to {
                let share = if to > from { f32::from(at - from) / f32::from(to - from) } else { 0.0 };
                table[usize::from(at)] = low + (high - low) * share;
            }
        }
        self.curves.insert(number, table);
    }

    fn header(&mut self, name: &str) {
        self.finish_region();
        self.finish_curve();
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
            "curve" => {
                self.curve = Some((None, Vec::new()));
                Level::Curve
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
            Level::Curve => {
                let Some((number, points)) = &mut self.curve else { return };
                match (name.as_str(), name.strip_prefix('v').and_then(|at| at.parse::<u8>().ok())) {
                    ("curve_index", _) => *number = number_of(&value).map(|index| index.max(0.0) as u16),
                    (_, Some(at)) if at <= 127 => points.push((at, number_of(&value).unwrap_or(0.0))),
                    _ => {}
                }
            }
            Level::Other => {}
        }
    }

    fn control(&mut self, name: &str, value: &str) {
        let (stem, number) = split_number(name);
        match (stem, number) {
            ("default_path", None) => self.default_path = value.replace('\\', "/"),
            ("note_offset", None) => self.key_offset += integer(value).unwrap_or(0),
            ("octave_offset", None) => self.key_offset += 12 * integer(value).unwrap_or(0),
            ("set_cc", Some(controller)) if STANDING.contains(&controller) => {
                self.standing.insert(controller, (number_of(value).unwrap_or(0.0) / 127.0).clamp(0.0, 1.0));
            }
            ("set_hdcc", Some(controller)) if STANDING.contains(&controller) => {
                self.standing.insert(controller, number_of(value).unwrap_or(0.0).clamp(0.0, 1.0));
            }
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
        let Building {
            mut region,
            sample,
            label,
            curve,
            mut disabled,
            no_key,
            filter_envelope,
            filter_depth,
            tune,
            transpose,
            volumes,
            curves,
            stood,
        } = building;
        region.tune = tune + 100.0 * transpose;
        region.volume += volumes.iter().sum::<f32>();
        // No key plays it: it is left out, unless a controller sets it off.
        disabled |= no_key && region.set_off.is_none();
        // What the controllers that stand where the file put them come to.
        for (stem, controller, amount) in &stood {
            let position = self.standing.get(controller).copied().unwrap_or(0.0);
            match stem.as_str() {
                "locc" => disabled |= position * 127.0 < amount - 0.5,
                "hicc" => disabled |= position * 127.0 > amount + 0.5,
                "amplitude_oncc" => region.amplitude *= (amount / 100.0 * position).max(0.0),
                "volume_oncc" => region.volume += amount * position,
                "pan_oncc" => region.pan = (region.pan + amount / 100.0 * position).clamp(-1.0, 1.0),
                _ => region.tune += amount * position,
            }
        }
        // What a controller moves is read along the curve that was named
        // for it, wherever in the file that was.
        for control in &mut region.controls {
            let named = curves
                .iter()
                .rev()
                .find(|(moved, controller, _)| (*moved, *controller) == (Some(control.moved), control.controller));
            control.curve = named.map_or(0, |(_, _, curve)| *curve);
        }

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
        // The filter has an envelope where something says how far it opens
        // the filter: the file, or a controller.
        let opened_by_controller = region.controls.iter().any(|control| control.moved == Moved::FilterEnvelope);
        if let Some(filter) = &mut region.filter
            && (filter_depth != 0.0 || opened_by_controller)
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
            // A key under the keyboard is no key: nothing plays it, unless
            // a controller sets the region off.
            "lokey" | "hikey" if number.is_some_and(|key| key < 0.0) => building.no_key = true,
            "lokey" => region.keys.0 = key.unwrap_or(0),
            "hikey" => region.keys.1 = key.unwrap_or(127),
            "key" if number.is_some_and(|key| key < 0.0) => building.no_key = true,
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
            "polyphony" => region.polyphony = Some(whole(amount).clamp(1, 256) as u16),
            "sustain_sw" => self.instrument.pedal = !value.trim().eq_ignore_ascii_case("off"),
            "sustain_cc" if (0.0..=127.0).contains(&amount) => self.instrument.pedal_controller = amount as u8,

            "tune" | "pitch" => building.tune = amount,
            "transpose" => building.transpose = amount,
            "pitch_keytrack" => region.pitch_by_key = amount,
            "pitch_veltrack" => region.pitch_by_velocity = amount,
            "pitch_random" => region.pitch_random = amount.abs(),
            "bend_up" | "bendup" => region.bend_up = amount,
            "bend_down" | "benddown" => region.bend_down = amount,

            "volume" | "gain" => region.volume = amount,
            // The levels above a region each have a volume of their own,
            // and they add up.
            "global_volume" => building.volumes[0] = amount,
            "master_volume" => building.volumes[1] = amount,
            "group_volume" => building.volumes[2] = amount,
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
            | "global_label" | "script" | "md5" | "ampeg_dynamic" | "off_shape" => {}
            _ if name.starts_with("hint_") => {}
            // Every controller is smoothed here, the same for all, and so
            // is the wheel.
            _ if name.contains("_smoothcc") || name.contains("_stepcc") || name == "bend_smooth" => {}
            // A region that a controller sets off, not a key.
            _ if name.starts_with("on_lo") || name.starts_with("on_hi") => {
                let (low, rest) = match name.strip_prefix("on_lo") {
                    Some(rest) => (true, rest),
                    None => (false, &name["on_hi".len()..]),
                };
                let (fine, digits) = match rest.strip_prefix("hdcc") {
                    Some(digits) => (true, digits),
                    None => (false, rest.strip_prefix("cc").unwrap_or("")),
                };
                let at = if fine { amount * 127.0 } else { amount };
                match digits.parse::<u32>() {
                    Ok(controller) if controller < 128 => {
                        let span = region.set_off.get_or_insert((controller as u8, 0.0, 127.0));
                        if u32::from(span.0) != controller {
                            // Two controllers for one region: only one sets it off here.
                            return false;
                        }
                        if low {
                            span.1 = at;
                        } else {
                            span.2 = at;
                        }
                        region.trigger = Trigger::Controller;
                    }
                    // The pressure on one key: nothing here moves it, so
                    // what it would set off is never set off.
                    Ok(note::KEY_PRESSURE) => building.disabled = true,
                    _ => {
                        building.disabled = true;
                        return false;
                    }
                }
            }
            _ if name.starts_with("lfo") && name.as_bytes().get(3).is_some_and(u8::is_ascii_digit) => {
                return wave_pair(building, name, amount, value);
            }
            _ => return self.numbered(building, name, amount, value),
        }
        true
    }

    /// The pairs whose name ends in a number: a controller's, or a point
    /// of a curve.
    fn numbered(&mut self, building: &mut Building, name: &str, amount: f32, value: &str) -> bool {
        // How far the filter's envelope opens it is a controller's to move
        // like anything else of the region, not a stage of the envelope.
        let depth_of_envelope = name.starts_with("fileg_depth") && name.contains("cc");
        // The envelopes, with and without a controller.
        for (prefix, filter) in [("ampeg_", false), ("fileg_", true)] {
            if depth_of_envelope {
                break;
            }
            let Some(rest) = name.strip_prefix(prefix) else { continue };
            let envelope = if filter { &mut building.filter_envelope } else { &mut building.region.envelope };
            return envelope_pair(envelope, rest, amount);
        }
        let (stem, number) = split_number(name);
        let Some(number) = number else { return false };
        // Which curve a controller is read along, for something it moves.
        if let Some(moved) = stem.strip_suffix("_curvecc") {
            let moved = match moved {
                "volume" | "gain" => Moved::Volume,
                "amplitude" => Moved::Amplitude,
                "pan" => Moved::Pan,
                "tune" | "pitch" => Moved::Tune,
                "cutoff" => Moved::Cutoff,
                "pitchlfo_depth" => Moved::Vibrato,
                "amplfo_depth" => Moved::Tremolo,
                "amp_veltrack" => Moved::Dynamics,
                "width" => Moved::Width,
                "offset" => Moved::Offset,
                "delay" => Moved::Delay,
                "resonance" => Moved::Resonance,
                "fileg_depth" => Moved::FilterEnvelope,
                _ => return false,
            };
            if !note::is_controller(number) {
                return false;
            }
            building.curves.push((Some(moved), number as u8, amount.max(0.0) as u16));
            return true;
        }
        let region = &mut building.region;
        if stem == "amp_velcurve_" {
            if number <= 127 {
                building.curve.push((number as u8, amount.clamp(0.0, 1.0)));
            }
            return true;
        }
        if STANDING.contains(&number) || number == note::KEY_PRESSURE {
            // One that stands where the file put it (and the pressure on
            // one key, which nothing here moves: it stands at rest): what
            // it does is worked out when the region is whole.
            let stem = match canonical(stem).as_str() {
                "locc" | "hicc" | "amplitude_oncc" | "volume_oncc" | "pan_oncc" | "tune_oncc" => canonical(stem),
                "lohdcc" | "hihdcc" => canonical(stem).replace("hd", ""),
                "gain_oncc" => "volume_oncc".to_string(),
                "pitch_oncc" => "tune_oncc".to_string(),
                _ => return false,
            };
            let amount = if canonical(name_stem(name)).contains("hdcc") { amount * 127.0 } else { amount };
            building.stood.retain(|(known, controller, _)| !(*known == stem && *controller == number));
            building.stood.push((stem, number, amount));
            return true;
        }
        if !note::is_controller(number) {
            // Other controllers above 127 are a player's own: the pitch
            // wheel, the pressure, how far a note is from the one before.
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
            // A span of a controller's travel, in MIDI's 0..=127 or as a
            // share of it.
            "locc" | "hicc" | "lohdcc" | "hihdcc" => {
                // One that goes both ways is in the span when nothing is said.
                let lowest = if controller == note::RANDOM_EITHER_WAY { -127.0 } else { 0.0 };
                if !region.when.iter().any(|(number, _, _)| *number == controller) {
                    region.when.push((controller, lowest, 127.0));
                }
                let span = region.when.iter_mut().find(|(number, _, _)| *number == controller).expect("just put");
                let amount = if stem.contains("hd") { amount * 127.0 } else { amount };
                if stem.starts_with("lo") {
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
            "amp_veltrack_oncc" => Moved::Dynamics,
            "width_oncc" => Moved::Width,
            "offset_oncc" => Moved::Offset,
            "delay_oncc" => Moved::Delay,
            "resonance_oncc" => Moved::Resonance,
            "fileg_depth_oncc" => Moved::FilterEnvelope,
            _ => return false,
        };
        let _ = value;
        match moved {
            Moved::Vibrato => drop(region.vibrato.get_or_insert_default()),
            Moved::Tremolo => drop(region.tremolo.get_or_insert_default()),
            Moved::Cutoff | Moved::Resonance | Moved::FilterEnvelope => drop(region.filter.get_or_insert_default()),
            _ => {}
        }
        region.controls.retain(|control| !(control.controller == controller && control.moved == moved));
        region.controls.push(Control { controller, moved, depth: amount, curve: 0 });
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
    /// Its keys are under the keyboard: no key plays it.
    no_key: bool,
    tune: f32,
    transpose: f32,
    filter_envelope: Envelope,
    filter_depth: f32,
    /// The volumes of the levels above it: global, master, group.
    volumes: [f32; 3],
    /// The curves named for what controllers move: what, which
    /// controller, which curve.
    curves: Vec<(Option<Moved>, u8, u16)>,
    /// What is said of controllers that stand where the file put them:
    /// the pair's name without its number, the controller, the amount.
    stood: Vec<(String, u32, f32)>,
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
    // The curve a controller is read along for a stage it lengthens.
    if let (Some(stage), Some(controller)) = (stem.strip_suffix("_curvecc").and_then(stage_of), number) {
        let found = envelope
            .by_controller
            .iter_mut()
            .find(|(which, number, _, _)| *which == stage && u32::from(*number) == controller);
        return match found {
            Some(entry) => {
                entry.3 = amount.max(0.0) as u16;
                true
            }
            // Said before the amount it belongs to: kept with nothing to move yet.
            None if note::is_controller(controller) => {
                envelope.by_controller.push((stage, controller as u8, 0.0, amount.max(0.0) as u16));
                true
            }
            None => false,
        };
    }
    let word = stem.strip_suffix("_oncc").or_else(|| stem.strip_suffix("cc"));
    match (word.and_then(stage_of), number) {
        (Some(stage), Some(controller)) if note::is_controller(controller) => {
            let controller = controller as u8;
            let curve = envelope
                .by_controller
                .iter()
                .find(|(which, number, _, _)| *which == stage && *number == controller)
                .map_or(0, |(_, _, _, curve)| *curve);
            envelope.by_controller.retain(|(which, number, _, _)| !(*which == stage && *number == controller));
            envelope.by_controller.push((stage, controller, scaled(stage), curve));
            true
        }
        _ => false,
    }
}

/// One pair of a numbered slow wave: `lfo01_freq`, `lfo2_pitch_oncc111`,
/// `lfo03_freq_lfo01_oncc118`, `lfo01_pitch_curvecc111`.
fn wave_pair(building: &mut Building, name: &str, amount: f32, value: &str) -> bool {
    let rest = &name[3..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let Ok(number) = rest[..digits].parse::<u8>() else { return false };
    let Some(rest) = rest[digits..].strip_prefix('_') else { return false };
    // What the pair is about, and the controller that moves it if one does.
    let (stem, last) = split_number(rest);
    let (word, controller, names_curve) = match (stem.strip_suffix("_curvecc"), last) {
        (Some(word), Some(controller)) => (word, Some(controller), true),
        _ => match (stem.strip_suffix("_oncc").or_else(|| stem.strip_suffix("_cc")), last) {
            (Some(word), Some(controller)) => (word, Some(controller), false),
            _ => (rest, None, false),
        },
    };
    let kind = match (word, integer(value)) {
        ("wave", Some(0)) => Some(Wave::Triangle),
        ("wave", Some(1)) => Some(Wave::Sine),
        ("wave", Some(3)) => Some(Wave::Square),
        ("wave", Some(6)) => Some(Wave::Ramp),
        ("wave", Some(7)) => Some(Wave::Saw),
        ("wave", Some(-1 | 12)) => Some(Wave::Random),
        ("wave", _) => return false,
        _ => None,
    };
    let swung = match word {
        "wave" => Swung::Rate,
        "freq" => Swung::Rate,
        "delay" => Swung::Delay,
        "fade" => Swung::Fade,
        "phase" => Swung::Phase,
        "pitch" => Swung::Pitch,
        "volume" => Swung::Volume,
        "cutoff" => Swung::Cutoff,
        "pan" => Swung::Pan,
        other => match other.strip_prefix("freq_lfo").and_then(|of| of.parse::<u8>().ok()) {
            Some(of) => Swung::RateOf(of),
            None => return false,
        },
    };
    let controller = match controller {
        Some(controller) if note::is_controller(controller) => Some(controller as u8),
        Some(_) => return false,
        None => None,
    };
    let region = &mut building.region;
    if !region.waves.iter().any(|wave| wave.number == number) {
        if region.waves.len() >= MAX_WAVES {
            return false;
        }
        region.waves.push(SlowWave { number, ..SlowWave::default() });
    }
    let wave = region.waves.iter_mut().find(|wave| wave.number == number).expect("it was just put there");
    if let Some(kind) = kind {
        wave.wave = kind;
        return true;
    }
    let known = wave.swings.iter_mut().find(|swing| swing.swung == swung && swing.controller == controller);
    match (known, names_curve) {
        (Some(swing), true) => swing.curve = amount.max(0.0) as u16,
        (Some(swing), false) => swing.amount = amount,
        // A curve said before the amount it belongs to moves nothing yet.
        (None, true) => wave.swings.push(Swing { swung, controller, amount: 0.0, curve: amount.max(0.0) as u16 }),
        (None, false) => wave.swings.push(Swing { swung, controller, amount, curve: 0 }),
    }
    if swung == Swung::Cutoff {
        region.filter.get_or_insert_default();
    }
    true
}

/// A name without the number it ends in.
fn name_stem(name: &str) -> &str {
    split_number(name).0
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
    const AS_WRITTEN: &[&str] =
        &["locc", "hicc", "lohdcc", "hihdcc", "xfin_locc", "xfin_hicc", "xfout_locc", "xfout_hicc"];
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
    let (stem, number) = split_number(name);
    if stem.ends_with("cc") && number.is_some_and(|number| !note::is_controller(number)) {
        return "what its maker's player makes up as controllers beyond the ones here (how fast a key came up, how \
                far a note is from the one before)";
    }
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
    } else if starts(&["sostenuto", "sustain_", "note_selfmask", "rt_dead"]) {
        "its rules for pedals and for which of a key's voices give way"
    } else if starts(&["lobend", "hibend", "lochanaft", "hichanaft", "lopolyaft", "hipolyaft", "lobpm", "hibpm"])
        || starts(&["lotimer", "hitimer", "sw_vel", "lohdcc", "hihdcc", "loprog", "hiprog", "pitch_keycenter"])
    {
        "conditions on which recording sounds (the pitch wheel, the pressure, the tempo, a file's own root note)"
    } else if starts(&["bend_step", "bend_smooth"]) {
        "how smoothly or in what steps the pitch wheel moves"
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
    fn a_notes_own_controllers_numbered_waves_and_what_begins_a_note_are_read() {
        let mut reader = Reader::default();
        let region = reader
            .region_of(&[
                ("sample", "a.wav"),
                ("locc131", "64"),
                ("hihdcc135", "0.5"),
                ("amplitude_oncc131", "100"),
                ("offset_cc131", "4800"),
                ("offset_curvecc131", "1"),
                ("delay_oncc135", "0.02"),
                ("resonance_cc20", "6"),
                ("fileg_depthcc21", "1200"),
                ("bend_smooth", "50"),
                ("lfo01_freq", "2"),
                ("lfo01_freq_oncc112", "8"),
                ("lfo1_pitch_oncc111", "22"),
                ("lfo01_pitch_curvecc111", "4"),
                ("lfo01_phase", "0.25"),
                ("lfo2_freq", "0.01"),
                ("lfo02_wave", "-1"),
                ("lfo2_freq_lfo1_oncc119", "3"),
                ("lfo02_volume", "1.5"),
                ("lfo02_delay_oncc116", "1"),
                ("lfo02_freq_smoothcc117", "20"),
                // What is not done, and stays said.
                ("lfo02_eq1gain_oncc115", "6"),
                ("lfo02_wave", "9"),
                ("tune_oncc140", "100"),
                ("locc132", "1"),
            ])
            .unwrap();
        assert_eq!(region.when, [(131, 64.0, 127.0), (135, 0.0, 63.5)]);
        let moved = |moved: Moved| region.controls.iter().find(|control| control.moved == moved).copied();
        assert_eq!(
            moved(Moved::Amplitude),
            Some(Control { controller: 131, moved: Moved::Amplitude, depth: 100.0, curve: 0 })
        );
        assert_eq!(
            moved(Moved::Offset),
            Some(Control { controller: 131, moved: Moved::Offset, depth: 4_800.0, curve: 1 })
        );
        assert_eq!(moved(Moved::Delay).map(|control| (control.controller, control.depth)), Some((135, 0.02)));
        assert_eq!(moved(Moved::Resonance).map(|control| (control.controller, control.depth)), Some((20, 6.0)));
        assert_eq!(
            moved(Moved::FilterEnvelope).map(|control| (control.controller, control.depth)),
            Some((21, 1_200.0))
        );
        assert!(region.filter.is_some(), "what moves a filter makes one");
        assert!(Moved::Offset.at_the_start() && Moved::Delay.at_the_start() && !Moved::Resonance.at_the_start());

        assert_eq!(region.waves.len(), 2, "lfo1 and lfo01 are one wave");
        let (first, second) = (&region.waves[0], &region.waves[1]);
        assert_eq!((first.number, first.wave, second.number, second.wave), (1, Wave::Sine, 2, Wave::Random));
        let swing = |swung, controller, amount, curve| Swing { swung, controller, amount, curve };
        assert_eq!(
            first.swings,
            [
                swing(Swung::Rate, None, 2.0, 0),
                swing(Swung::Rate, Some(112), 8.0, 0),
                swing(Swung::Pitch, Some(111), 22.0, 4),
                swing(Swung::Phase, None, 0.25, 0),
            ]
        );
        assert_eq!(
            second.swings,
            [
                swing(Swung::Rate, None, 0.01, 0),
                swing(Swung::RateOf(1), Some(119), 3.0, 0),
                swing(Swung::Volume, None, 1.5, 0),
                swing(Swung::Delay, Some(116), 1.0, 0),
            ]
        );
        let mut said: Vec<&str> = reader.unsupported.keys().map(String::as_str).collect();
        said.sort_unstable();
        assert_eq!(said, ["lfo02_eq1gain_oncc115", "lfo02_wave", "locc132", "tune_oncc140"]);
        assert!(missed("tune_oncc140").contains("how far a note is from the one before"));
        assert!(
            note::is_controller(127)
                && note::is_controller(137)
                && !note::is_controller(132)
                && !note::is_controller(400)
        );

        // No more waves than a voice has room for.
        let pairs: Vec<(String, &str)> = (1..=7).map(|number| (format!("lfo{number}_freq"), "1")).collect();
        let mut pairs: Vec<(&str, &str)> = pairs.iter().map(|(name, value)| (name.as_str(), *value)).collect();
        pairs.push(("sample", "a.wav"));
        let mut reader = Reader::default();
        assert_eq!(reader.region_of(&pairs).unwrap().waves.len(), MAX_WAVES);
        assert_eq!(reader.unsupported.keys().collect::<Vec<_>>(), ["lfo7_freq"]);
    }

    #[test]
    fn a_controller_only_its_makers_window_moves_stands_where_the_file_puts_it() {
        // A kit recorded through three microphones, each with a level of its own in its maker's window.
        let kit = read_text(
            "<control> set_hdcc200=0.5 set_cc201=0 set_hdcc202=1 set_cc300=64\n\
             <global> key=36 amp_veltrack=0\n\
             <region> sample=close.wav locc200=1 amplitude_oncc200=100 volume=-3 volume_oncc202=-6 pan_oncc300=-100\n\
             <region> sample=room.wav locc201=1 amplitude_oncc201=100\n\
             <region> sample=overhead.wav lohdcc202=0.5 amplitude_oncc202=100 tune_oncc202=50 tune=10\n\
             <region> sample=never.wav amplitude_oncc203=100 hicc200=10\n\
             <region> sample=moved.wav cutoff_oncc200=1200 amplitude_oncc129=100",
        )
        .unwrap();
        let named: Vec<&str> =
            kit.regions.iter().map(|region| kit.recordings[region.recording.unwrap() as usize].as_str()).collect();
        assert_eq!(named, ["close.wav", "overhead.wav", "moved.wav"], "a microphone that is turned off is not played");
        let (close, overhead) = (&kit.regions[0], &kit.regions[1]);
        assert_eq!((close.amplitude, close.volume), (0.5, -9.0), "half its level, and 6 dB down for the third's");
        assert!((close.pan + 64.0 / 127.0).abs() < 1e-6, "as far to the left as the fourth stands: {}", close.pan);
        assert_eq!((overhead.amplitude, overhead.tune), (1.0, 60.0));
        assert!(kit.regions.iter().all(|region| region.controls.is_empty() && region.when.is_empty()));
        // What else is asked of one is said; so is a controller that is a player's own.
        let said: Vec<&str> = kit.unsupported.iter().map(|un| un.name.as_str()).collect();
        assert_eq!(said, ["amplitude_oncc129", "cutoff_oncc200"]);
    }

    #[test]
    fn what_a_controller_sets_off_is_read_with_its_span_and_the_pressure_on_a_key_stands_at_rest() {
        // A piano with the noise of its pedal, and a cymbal that a drummer chokes by leaning on its pad.
        let piano = read_text(
            "<region> sample=note.wav key=60\n\
             <group> lokey=-1 hikey=-1 volume=-20 group=1 on_locc64=127 on_hicc64=127 locc64=127 hicc64=127\n\
             <region> sample=down1.wav lorand=0 hirand=0.5 <region> sample=down2.wav lorand=0.5 hirand=1\n\
             <group> key=-1 pitch_keycenter=48 on_lohdcc64=0 on_hihdcc64=0.25 <region> sample=up.wav\n\
             <group> key=-1 <region> sample=nothing-plays-this.wav\n\
             <group> key=49 group=2 <region> sample=*silence locc130=127 hicc130=127 on_locc130=127 on_hicc130=127\n\
             <region> sample=crash.wav key=49 off_by=2 locc130=0 hicc130=126\n\
             <region> sample=choked.wav key=49 locc130=64",
        )
        .unwrap();
        let named: Vec<&str> =
            piano.regions.iter().map(|region| piano.recordings[region.recording.unwrap() as usize].as_str()).collect();
        assert_eq!(named, ["note.wav", "down1.wav", "down2.wav", "up.wav", "crash.wav"]);
        let [note, down, _, up, crash] = piano.regions.as_slice() else { panic!() };
        assert_eq!((note.trigger, note.set_off), (Trigger::Attack, None));
        // The pedal coming all the way down sets its noise off, wherever the keys are.
        assert_eq!((down.trigger, down.set_off, down.volume), (Trigger::Controller, Some((64, 127.0, 127.0)), -20.0));
        assert_eq!((down.when.as_slice(), down.random), (&[(64u8, 127.0f32, 127.0f32)][..], (0.0, 0.5)));
        // In hundredths of its travel, too; and on the key its recording was made on.
        assert_eq!((up.set_off, up.root), (Some((64, 0.0, 31.75)), 48));
        // The pressure on a key stands at rest: the cymbal plays, at its own level, and nothing is said.
        assert_eq!((crash.keys, crash.off_by, crash.amplitude), ((49, 49), Some(2), 1.0));
        assert!(crash.when.is_empty() && crash.controls.is_empty());
        assert!(piano.unsupported.is_empty(), "{:?}", piano.unsupported);
        // The pedal is one of the controllers it listens to.
        assert!(piano.summary().controllers.iter().any(|(number, _)| *number == 64));
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
        assert_eq!(soft.controls, [Control { controller: 1, moved: Moved::Vibrato, depth: 30.0, curve: 0 }]);
        assert_eq!(loud.envelope.by_controller, [(Stage::Attack, 73, 2.0, 0)]);
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
            "<control> hint_ram_based=1 set_cc140=1\n<global> eq1_gain=3 eq1_freq=80 eg01_time1=4 lfo01_pitch=10\n\
             <effect> type=reverb\n<midi> x=1\n\
             <group> key=36 loop_mode=one_shot off_by=2 group=1 off_mode=time off_time=0.2 cutoff2=500\n\
             <region> sample=kick.wav on_locc129=1 cutoff=900 fil_type=lpf_2p fil_veltrack=1200 fileg_depth=2400 fileg_decay=0.1\n\
             <region> sample=*sine key=40\n<region> sample=*silence key=41 group=2\n\
             <region> sample=snare.wav key=38 direction=reverse trigger=whenever gain_cc7=-6 amplitude_cc11=100",
        )
        .unwrap();
        let named: Vec<(&str, usize)> = kit.unsupported.iter().map(|un| (un.name.as_str(), un.count)).collect();
        for expected in [
            ("eq1_gain", 4),
            ("eq1_freq", 4),
            ("eg01_time1", 4),
            ("<effect>", 1),
            ("<midi>", 1),
            ("cutoff2", 4),
            ("on_locc129", 1),
            ("sample=*sine", 1),
            ("direction", 1),
            ("trigger", 1),
            ("set_cc140", 1),
        ] {
            assert!(named.contains(&expected), "{expected:?} is not among {named:?}");
        }
        assert!(!named.iter().any(|(name, _)| name.starts_with("hint") || *name == "cutoff" || *name == "type"));
        let effects = kit.unsupported.iter().find(|un| un.name == "<effect>").unwrap();
        assert!(effects.miss.contains("effects of its own"));
        assert!(kit.unsupported.iter().find(|un| un.name == "eq1_gain").unwrap().miss.contains("equalisers"));
        // And what it does do is all there.
        // A region that a controller of a player's own would set off is left out: nothing here moves it.
        let [silence, snare] = kit.regions.as_slice() else { panic!("{}", kit.regions.len()) };
        let kick = read_text(
            "<group> key=36 off_by=2 group=1 off_mode=time off_time=0.2\n\
             <region> sample=kick.wav cutoff=900 fil_type=lpf_2p fil_veltrack=1200 fileg_depth=2400 fileg_decay=0.1",
        )
        .unwrap();
        let kick = &kick.regions[0];
        assert_eq!((kick.keys, kick.off_by, kick.group, kick.off_mode), ((36, 36), Some(2), 1, OffMode::Time(0.2)));
        let filter = kick.filter.as_ref().unwrap();
        assert_eq!((filter.kind, filter.cutoff, filter.by_velocity), (FilterKind::LowPass2, 900.0, 1_200.0));
        let (envelope, depth) = filter.envelope.as_ref().unwrap();
        assert_eq!((envelope.decay, *depth), (0.1, 2_400.0));
        assert_eq!((silence.recording, silence.group), (None, 2));
        assert_eq!(snare.controls.len(), 2);
        assert_eq!(snare.controls[0], Control { controller: 7, moved: Moved::Volume, depth: -6.0, curve: 0 });
        assert_eq!(snare.controls[1], Control { controller: 11, moved: Moved::Amplitude, depth: 100.0, curve: 0 });
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
