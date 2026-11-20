//! A plugin's parameters, as VST3 has them and as the device API wants
//! them.
//!
//! Every VST3 parameter is a number from 0 to 1. What that means in plain
//! terms (which step of a switch, which entry of a list) only the plugin's
//! edit controller can say, and it may only be asked on the main thread.
//! The device API wants plain values that the audio thread can hand to the
//! plugin, so plain values are chosen here such that turning them into
//! normalized ones is arithmetic: a parameter with steps counts them, and
//! one without is used as it is, 0 to 1.

use super::text;
use crate::param_text::{self, Values};
use std::collections::{HashMap, HashSet};
use trill_core::device::ControllerSupport;
use trill_core::device::{ParamInfo, ParamKind};
use trill_core::{Channel, ControllerKind};
use vst3::ComPtr;
use vst3::Steinberg::Vst::ControllerNumbers_::{kCtrlAllNotesOff, kCtrlAllSoundsOff};
use vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::{kCanAutomate, kIsHidden, kIsReadOnly};
use vst3::Steinberg::Vst::{
    CtrlNumber, IEditController, IEditControllerTrait, IMidiMapping, IMidiMappingTrait, IUnitInfo, IUnitInfoTrait,
    ParamID, ParameterInfo, String128, UnitID, UnitInfo, kRootUnitId,
};
use vst3::Steinberg::kResultOk;

/// The most steps a parameter may have and still be listed by its steps'
/// names. With more, the names say less than the number.
const MAX_CHOICES: u32 = 64;
/// Upper bounds on what is read from a plugin, so that a plugin reporting
/// nonsense counts cannot make the host loop for minutes.
const MAX_PARAMS: i32 = 1 << 20;
const MAX_UNITS: i32 = 1 << 12;
/// How many parents of a unit are followed when naming it. Real plugins
/// nest two or three deep; the limit stops a cycle.
const MAX_UNIT_DEPTH: usize = 8;

/// How a parameter's plain values relate to its normalized ones.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mapping {
    /// The plain value is the normalized one, 0 to 1.
    Continuous,
    /// The plain value is the step, 0 to `steps`.
    Stepped { steps: u32 },
}

impl Mapping {
    /// `step_count` is what the plugin reports: the number of steps above
    /// the lowest, or 0 for a continuous parameter.
    fn of(step_count: i32) -> Self {
        match u32::try_from(step_count) {
            Ok(steps) if steps > 0 => Mapping::Stepped { steps },
            _ => Mapping::Continuous,
        }
    }

    /// The normalized value for a plain one. Realtime-safe.
    pub(crate) fn normalize(self, plain: f64) -> f64 {
        let plain = if plain.is_finite() { plain } else { 0.0 };
        match self {
            Mapping::Continuous => plain.clamp(0.0, 1.0),
            Mapping::Stepped { steps } => plain.round().clamp(0.0, f64::from(steps)) / f64::from(steps),
        }
    }

    /// The plain value for a normalized one, by the SDK's rule for
    /// discrete parameters: the range is cut into `steps + 1` equal parts.
    pub(crate) fn plain(self, normalized: f64) -> f64 {
        let normalized = if normalized.is_finite() { normalized.clamp(0.0, 1.0) } else { 0.0 };
        match self {
            Mapping::Continuous => normalized,
            Mapping::Stepped { steps } => (normalized * f64::from(steps + 1)).floor().min(f64::from(steps)),
        }
    }

    fn max(self) -> f64 {
        match self {
            Mapping::Continuous => 1.0,
            Mapping::Stepped { steps } => f64::from(steps),
        }
    }
}

/// One parameter the host may set.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParamSpec {
    pub id: ParamID,
    /// Its place among all of the plugin's parameters.
    pub slot: usize,
    pub info: ParamInfo,
    pub mapping: Mapping,
    /// The value the plugin itself calls the default, in plain units.
    /// `info.default` is replaced by the value the instance starts with.
    pub declared: f64,
    /// What the plugin's values are measured in, if it says: kept for
    /// every kind of parameter, to read a value's text with.
    pub units: String,
}

/// The parameters of a plugin instance.
#[derive(Default)]
pub(crate) struct ParamTable {
    /// The id of every parameter the plugin has, listed or not, in the
    /// plugin's order. Values pass between its two halves for all of them.
    pub ids: Vec<ParamID>,
    /// The parameters a device exposes: those a host may automate. Their
    /// order is the one the processor's `set_param` indexes.
    pub listed: Vec<ParamSpec>,
}

/// A parameter as the plugin describes it.
struct Described {
    id: ParamID,
    slot: usize,
    title: String,
    short_title: String,
    units: String,
    step_count: i32,
    default_normalized: f64,
    unit: UnitID,
}

/// Whether a device exposes a parameter with these flags: one that can be
/// automated, is meant to be seen, and is the host's to set.
fn is_listed(flags: i32) -> bool {
    flags & kCanAutomate != 0 && flags & (kIsHidden | kIsReadOnly) == 0
}

/// Reads the plugin's parameter list. Parameters the plugin fails to
/// describe, and repeated ids, are left out: values are addressed by id,
/// so every entry must be the only one with its id.
pub(crate) fn read(controller: &ComPtr<IEditController>, plugin: &str) -> ParamTable {
    let mut table = ParamTable::default();
    let mut seen = HashSet::new();
    let mut described = Vec::new();
    // SAFETY: `controller` is a live edit controller, used on the plugin
    // main thread.
    let count = unsafe { controller.getParameterCount() }.clamp(0, MAX_PARAMS);
    for index in 0..count {
        let mut info = ParameterInfo {
            id: 0,
            title: [0; 128],
            shortTitle: [0; 128],
            units: [0; 128],
            stepCount: 0,
            defaultNormalizedValue: 0.0,
            unitId: kRootUnitId,
            flags: 0,
        };
        // SAFETY: as above; `info` is a local the plugin fills in.
        if unsafe { controller.getParameterInfo(index, &mut info) } != kResultOk {
            log::debug!("[{plugin}] does not describe parameter {index}; skipping it");
            continue;
        }
        if !seen.insert(info.id) {
            log::warn!("[{plugin}] reports parameter id {} twice; keeping the first", info.id);
            continue;
        }
        let slot = table.ids.len();
        table.ids.push(info.id);
        if is_listed(info.flags) {
            described.push(Described {
                id: info.id,
                slot,
                title: text::from_utf16(&info.title),
                short_title: text::from_utf16(&info.shortTitle),
                units: text::from_utf16(&info.units),
                step_count: info.stepCount,
                default_normalized: info.defaultNormalizedValue,
                unit: info.unitId,
            });
        }
    }

    let names = unique_names(&described, &unit_paths(controller));
    table.listed = described
        .iter()
        .zip(names)
        .map(|(param, name)| spec(param, name, |steps| choice_names(controller, param.id, steps)))
        .collect();
    table
}

/// Builds the device API's view of a parameter. `choices` reads the names
/// of the steps of a parameter with that many of them, if the plugin can
/// name them.
fn spec(param: &Described, name: String, choices: impl FnOnce(u32) -> Option<Vec<String>>) -> ParamSpec {
    let mapping = Mapping::of(param.step_count);
    let kind = match mapping {
        Mapping::Continuous => ParamKind::Float,
        Mapping::Stepped { steps: 1 } => ParamKind::Bool,
        Mapping::Stepped { steps } if steps <= MAX_CHOICES => choices(steps).map_or(ParamKind::Int, ParamKind::Enum),
        Mapping::Stepped { .. } => ParamKind::Int,
    };
    // A switch or a list entry is not measured in anything.
    let unit = if matches!(kind, ParamKind::Float | ParamKind::Int) { param.units.clone() } else { String::new() };
    ParamSpec {
        id: param.id,
        slot: param.slot,
        info: ParamInfo {
            id: param.id.to_string(),
            name,
            min: 0.0,
            max: mapping.max(),
            default: mapping.plain(param.default_normalized),
            unit,
            kind,
        },
        mapping,
        declared: mapping.plain(param.default_normalized),
        units: param.units.clone(),
    }
}

/// What the plugin shows for the plain value `value` of a parameter, with
/// its unit where the plugin keeps that apart ("-6.0" and "dB").
pub(crate) fn text(controller: &ComPtr<IEditController>, spec: &ParamSpec, value: f64) -> Option<String> {
    let mut shown: String128 = [0; 128];
    // SAFETY: `controller` is a live edit controller, used on the plugin
    // main thread; `shown` is a local the plugin fills in.
    let result = unsafe { controller.getParamStringByValue(spec.id, spec.mapping.normalize(value), &mut shown) };
    let shown = text::from_utf16(&shown).trim().to_string();
    if result != kResultOk || shown.is_empty() {
        return None;
    }
    let units = spec.units.trim();
    // Some plugins put the unit into the text as well.
    let has_units = units.is_empty() || shown.to_lowercase().ends_with(&units.to_lowercase());
    Some(if has_units { shown } else { format!("{shown} {units}") })
}

/// The plain value of a parameter that the plugin shows as `wanted`: by
/// the plugin's own account if it gives one, and otherwise found among
/// what it shows for its values.
pub(crate) fn value_of(controller: &ComPtr<IEditController>, spec: &ParamSpec, wanted: &str) -> Option<f64> {
    let ask = |asked: &str| {
        let mut buffer = [0u16; 256];
        text::to_utf16(asked, &mut buffer);
        let mut normalized = f64::NAN;
        // SAFETY: `controller` is a live edit controller, used on the
        // plugin main thread; the text is NUL-terminated and `normalized`
        // a local the plugin fills in.
        let result = unsafe { controller.getParamValueByString(spec.id, buffer.as_mut_ptr(), &mut normalized) };
        (result == kResultOk && normalized.is_finite()).then(|| spec.mapping.plain(normalized))
    };
    let wanted = wanted.trim();
    // As it was typed, and without the unit, which plugins that keep the
    // unit apart do not expect in the text.
    let units = spec.units.trim();
    let bare = wanted.len().checked_sub(units.len()).filter(|_| !units.is_empty()).and_then(|at| {
        let (number, tail) = (wanted.get(..at)?, wanted.get(at..)?);
        tail.eq_ignore_ascii_case(units).then(|| number.trim_end())
    });
    let asked = ask(wanted).or_else(|| bare.and_then(ask));
    // A plugin's own answer counts only if it shows that value the way it
    // was asked for: asked for something they do not know, plugins answer
    // with the nearest end of the range, or with 0.
    let same = |shown: &str| {
        let canonical = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        canonical(shown) == canonical(wanted)
            || matches!((param_text::number(shown), param_text::number(wanted)),
                (Some((shown, unit)), Some((wanted, wanted_unit)))
                    if (wanted_unit.is_empty() || unit == wanted_unit)
                        && (shown - wanted).abs() <= 1e-9 * wanted.abs().max(1.0))
    };
    let confirmed = asked.filter(|value| text(controller, spec, *value).is_some_and(|shown| same(&shown)));
    let values = match spec.mapping {
        Mapping::Continuous => Values::Range(0.0, 1.0),
        Mapping::Stepped { steps } => Values::Steps(steps),
    };
    confirmed.or_else(|| param_text::find(wanted, values, |value| text(controller, spec, value)))
}

/// The name of each of the `steps + 1` values of a stepped parameter, or
/// `None` if the plugin does not name every one of them.
fn choice_names(controller: &ComPtr<IEditController>, id: ParamID, steps: u32) -> Option<Vec<String>> {
    (0..=steps)
        .map(|step| {
            let mut name: String128 = [0; 128];
            let normalized = f64::from(step) / f64::from(steps);
            // SAFETY: `controller` is a live edit controller, used on the
            // plugin main thread; `name` is a local the plugin fills in.
            let result = unsafe { controller.getParamStringByValue(id, normalized, &mut name) };
            let name = text::from_utf16(&name);
            (result == kResultOk && !name.is_empty()).then_some(name)
        })
        .collect()
}

/// A name for each parameter that no other parameter has.
///
/// Plugins reuse short titles across their sections ("Cutoff" in each
/// filter). Only those get the name of their unit, VST3's word for such a
/// section, put in front, so unambiguous names stay short. Names that are
/// still shared are numbered.
fn unique_names(params: &[Described], units: &HashMap<UnitID, String>) -> Vec<String> {
    fn uses<'a>(names: impl Iterator<Item = &'a String>) -> HashMap<&'a str, usize> {
        let mut uses = HashMap::new();
        for name in names {
            *uses.entry(name.as_str()).or_default() += 1;
        }
        uses
    }

    let plain: Vec<String> = params
        .iter()
        .map(|param| match (param.title.is_empty(), param.short_title.is_empty()) {
            (false, _) => param.title.clone(),
            (true, false) => param.short_title.clone(),
            (true, true) => format!("Parameter {}", param.id),
        })
        .collect();
    let plain_uses = uses(plain.iter());
    let qualified: Vec<String> = params
        .iter()
        .zip(&plain)
        .map(|(param, name)| match units.get(&param.unit) {
            Some(unit) if plain_uses[name.as_str()] > 1 && !unit.is_empty() => format!("{unit}/{name}"),
            _ => name.clone(),
        })
        .collect();
    let qualified_uses = uses(qualified.iter());
    let mut earlier: HashMap<&str, usize> = HashMap::new();
    qualified
        .iter()
        .map(|name| {
            let ordinal = earlier.entry(name.as_str()).or_default();
            *ordinal += 1;
            match (qualified_uses[name.as_str()], *ordinal) {
                (1, _) | (_, 1) => name.clone(),
                (_, ordinal) => format!("{name} ({ordinal})"),
            }
        })
        .collect()
}

/// The path of each of the plugin's units below its root, such as
/// `Oscillator 1/Filter`. Empty if the plugin has no units.
fn unit_paths(controller: &ComPtr<IEditController>) -> HashMap<UnitID, String> {
    let Some(units) = controller.cast::<IUnitInfo>() else { return HashMap::new() };
    // SAFETY: `units` is an interface of the live edit controller, used on
    // the plugin main thread; each `UnitInfo` is a local the plugin fills.
    let described: HashMap<UnitID, (UnitID, String)> = unsafe {
        (0..units.getUnitCount().clamp(0, MAX_UNITS))
            .filter_map(|index| {
                let mut info = UnitInfo { id: 0, parentUnitId: 0, name: [0; 128], programListId: 0 };
                (units.getUnitInfo(index, &mut info) == kResultOk)
                    .then(|| (info.id, (info.parentUnitId, text::from_utf16(&info.name))))
            })
            .collect()
    };
    described.keys().map(|&unit| (unit, unit_path(&described, unit))).collect()
}

/// `units` maps each unit to its parent and name. The root unit, which
/// every parameter without a section belongs to, has no path.
fn unit_path(units: &HashMap<UnitID, (UnitID, String)>, unit: UnitID) -> String {
    let mut names = Vec::new();
    let mut current = unit;
    for _ in 0..MAX_UNIT_DEPTH {
        let Some((parent, name)) = units.get(&current).filter(|_| current != kRootUnitId) else { break };
        if !name.is_empty() {
            names.push(name.as_str());
        }
        current = *parent;
    }
    names.reverse();
    names.join("/")
}

/// The current plain value of each parameter of `specs`, in their order.
pub(crate) fn values(controller: &ComPtr<IEditController>, specs: &[ParamSpec]) -> Vec<f64> {
    specs
        .iter()
        .map(|spec| {
            // SAFETY: `controller` is a live edit controller, used on the
            // plugin main thread.
            let normalized = unsafe { controller.getParamNormalized(spec.id) };
            if normalized.is_finite() { spec.mapping.plain(normalized) } else { spec.info.default }
        })
        .collect()
}

/// Replaces each parameter's declared default with the value it has now:
/// with the saved state restored, and before any value from the model is
/// applied.
///
/// The model stores only the parameters the user has set. For all others
/// the engine assumes the default, both to display them and to return to
/// when a setting is undone. What a plugin declares as default cannot serve
/// as that: it says nothing about the patch that is loaded.
pub(crate) fn read_baseline(controller: &ComPtr<IEditController>, specs: &mut [ParamSpec]) {
    let current = values(controller, specs);
    for (spec, value) in specs.iter_mut().zip(current) {
        spec.info.default = value;
    }
}

/// Maps model values (by VST3 parameter id, in plain units) to parameter
/// indices, dropping ids the plugin does not list and values that are not
/// numbers.
pub(crate) fn resolve_values(specs: &[ParamSpec], values: &[(ParamID, f64)]) -> Vec<(usize, f64)> {
    values
        .iter()
        .filter_map(|&(id, value)| {
            let index = specs.iter().position(|spec| spec.id == id)?;
            value.is_finite().then(|| (index, specs[index].info.clamp(value)))
        })
        .collect()
}

/// Channel pressure and the pitch wheel, as VST3 numbers them among the
/// MIDI controllers.
pub(crate) const AFTERTOUCH: u8 = 128;
pub(crate) const PITCH_BEND: u8 = 129;
/// The most controllers kept: every one a plugin could name.
const MIDI_CONTROLLERS: u8 = 120;
/// How many numbers there are to ask a plugin about on each channel: the
/// controllers, pressure and the wheel.
const NUMBERS: usize = PITCH_BEND as usize + 1;
/// What the table holds where the plugin named no parameter: the id VST3
/// keeps for "no parameter".
const NO_PARAM: ParamID = ParamID::MAX;

/// The parameters through which the plugin takes what a keyboard sends
/// beside notes: the pedal, the wheels, any other controller, on each MIDI
/// channel its event bus has.
///
/// A VST3 plugin is not sent MIDI controllers. One that understands them
/// names a parameter for each, by channel, and the host sets that
/// parameter instead. The plugin is asked once, when it is made, on the
/// thread plugins are made on: the audio thread looks the answer up.
///
/// **A channel the plugin has not got is its first.** A song may say more
/// channels than a plugin's event bus has (many say one). Whatever is on
/// such a channel, notes and controllers alike, goes to the plugin on its
/// first, as an instrument that listens to every channel takes it:
/// nothing is left out, and the plugin is never handed a channel it said
/// it has not got. A controller the plugin names a parameter for on its
/// first channel only is set there whatever channel the song has it on,
/// for the same reason.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MidiParams {
    /// How many channels the plugin's event bus has: none stands for one.
    channels: usize,
    /// The parameter for each number of each of those channels, a channel
    /// after the other, [`NO_PARAM`] where there is none. Empty for a
    /// plugin that names none at all.
    table: Vec<ParamID>,
}

impl MidiParams {
    /// A plugin that names no parameter, whose event bus has `channels`
    /// channels.
    pub(crate) fn plain(channels: usize) -> Self {
        Self { channels: channels.clamp(1, Channel::COUNT), table: Vec::new() }
    }

    /// The channel the plugin is told of what the song has on `channel`:
    /// that one where its event bus has it, and its first where not.
    pub(crate) fn channel(&self, channel: Channel) -> Channel {
        if channel.index() < self.channels { channel } else { Channel::FIRST }
    }

    /// The parameter that stands for `controller` on `channel`, if the
    /// plugin has one: the one it names for that channel, or else the one
    /// it names for its first.
    pub(crate) fn param(&self, channel: Channel, controller: u8) -> Option<ParamID> {
        let named = |channel: Channel| {
            let at = channel.index() * NUMBERS + usize::from(controller);
            self.table.get(at).copied().filter(|id| *id != NO_PARAM)
        };
        if usize::from(controller) >= NUMBERS {
            return None;
        }
        named(self.channel(channel)).or_else(|| named(Channel::FIRST))
    }

    /// The controllers the plugin has named a parameter for on its first
    /// channel, with the sustain pedal, which the host works out for a
    /// plugin that has not.
    pub(crate) fn support(&self) -> ControllerSupport {
        let mut kinds: Vec<ControllerKind> = (0..NUMBERS as u8)
            .filter(|number| self.param(Channel::FIRST, *number).is_some())
            .filter_map(|number| match number {
                AFTERTOUCH => Some(ControllerKind::Pressure),
                PITCH_BEND => Some(ControllerKind::PitchBend),
                number => ControllerKind::numbered(number),
            })
            .chain([ControllerKind::SUSTAIN])
            .collect();
        kinds.sort();
        kinds.dedup();
        ControllerSupport::Listed(kinds)
    }

    /// A plugin with one channel that names these parameters: controller
    /// number and parameter.
    #[cfg(test)]
    pub(crate) fn of(assigned: &[(u8, ParamID)]) -> Self {
        let on_first: Vec<(u8, u8, ParamID)> = assigned.iter().map(|(number, id)| (1, *number, *id)).collect();
        Self::on_channels(1, &on_first)
    }

    /// A plugin whose event bus has `channels` channels and that names
    /// these parameters: the channel as a person calls it, the controller
    /// number and the parameter.
    #[cfg(test)]
    pub(crate) fn on_channels(channels: usize, assigned: &[(u8, u8, ParamID)]) -> Self {
        let mut table = vec![NO_PARAM; channels * NUMBERS];
        for &(channel, number, id) in assigned {
            table[(usize::from(channel) - 1) * NUMBERS + usize::from(number)] = id;
        }
        Self { channels, table }
    }
}

/// Asks the plugin which parameters stand for the MIDI controllers,
/// pressure and the pitch wheel, on each of the `channels` channels of its
/// first event bus. Called where the plugin is made, never on an audio
/// thread.
pub(crate) fn midi_params(controller: &ComPtr<IEditController>, channels: usize) -> MidiParams {
    let plain = MidiParams::plain(channels);
    let Some(mapping) = controller.cast::<IMidiMapping>() else { return plain };
    let mut table = vec![NO_PARAM; plain.channels * NUMBERS];
    let mut named = false;
    for channel in 0..plain.channels {
        for number in (0..MIDI_CONTROLLERS).chain([AFTERTOUCH, PITCH_BEND]) {
            let mut id: ParamID = 0;
            // SAFETY: `mapping` is an interface of the live edit
            // controller, used on the plugin main thread; `id` is a local.
            let result =
                unsafe { mapping.getMidiControllerAssignment(0, channel as i16, CtrlNumber::from(number), &mut id) };
            if result == kResultOk && id != NO_PARAM {
                table[channel * NUMBERS + usize::from(number)] = id;
                named = true;
            }
        }
    }
    if named { MidiParams { table, ..plain } } else { plain }
}

/// The parameters through which the plugin takes MIDI's "all sound off"
/// and "all notes off".
///
/// A VST3 plugin is not sent MIDI controllers. One that understands them
/// names a parameter for each, and the host sets that parameter instead.
/// These two are how a host asks a plugin to fall silent.
///
/// They are asked for on each of the `channels` channels of the plugin's
/// event bus, and each is returned with its channel, as a plugin counts
/// them from 0: a plugin that keeps its channels apart is told on every
/// channel a song spoke on. A parameter that stands for more than one
/// channel is listed once, under the first of them.
pub(crate) fn silencing_params(controller: &ComPtr<IEditController>, channels: usize) -> Vec<(u8, ParamID)> {
    let Some(mapping) = controller.cast::<IMidiMapping>() else { return Vec::new() };
    let mut found: Vec<(u8, ParamID)> = Vec::new();
    for channel in 0..channels.clamp(1, Channel::COUNT) {
        for number in [kCtrlAllSoundsOff, kCtrlAllNotesOff] {
            let mut id: ParamID = 0;
            // SAFETY: `mapping` is an interface of the live edit
            // controller, used on the plugin main thread; `id` is a local.
            let result =
                unsafe { mapping.getMidiControllerAssignment(0, channel as i16, number as CtrlNumber, &mut id) };
            if result == kResultOk && !found.iter().any(|(_, known)| *known == id) {
                found.push((channel as u8, id));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn described(id: ParamID, title: &str, step_count: i32) -> Described {
        Described {
            id,
            slot: id as usize,
            title: title.to_string(),
            short_title: String::new(),
            units: "Hz".to_string(),
            step_count,
            default_normalized: 0.5,
            unit: kRootUnitId,
        }
    }

    fn no_names(_: u32) -> Option<Vec<String>> {
        None
    }

    #[test]
    fn only_automatable_visible_writable_parameters_are_listed() {
        assert!(is_listed(kCanAutomate));
        assert!(is_listed(kCanAutomate | 0x8), "a list parameter is still a parameter");
        assert!(!is_listed(0), "not automatable");
        assert!(!is_listed(kCanAutomate | kIsReadOnly));
        assert!(!is_listed(kCanAutomate | kIsHidden));
        assert!(!is_listed(kIsHidden | kIsReadOnly));
    }

    #[test]
    fn a_continuous_parameter_is_used_as_it_is() {
        let spec = spec(&described(7, "Cutoff", 0), "Cutoff".into(), no_names);
        assert_eq!(spec.mapping, Mapping::Continuous);
        assert_eq!((spec.id, spec.slot, spec.info.id.as_str(), spec.info.name.as_str()), (7, 7, "7", "Cutoff"));
        assert_eq!((spec.info.min, spec.info.max, spec.info.default), (0.0, 1.0, 0.5));
        assert_eq!((&spec.info.kind, spec.info.unit.as_str()), (&ParamKind::Float, "Hz"));

        assert_eq!(spec.mapping.normalize(0.25), 0.25);
        assert_eq!(spec.mapping.plain(0.25), 0.25);
        assert_eq!((spec.mapping.normalize(7.0), spec.mapping.normalize(-1.0)), (1.0, 0.0));
        assert_eq!((spec.mapping.normalize(f64::NAN), spec.mapping.plain(f64::INFINITY)), (0.0, 0.0));

        // A nonsense step count is no step count.
        assert_eq!(Mapping::of(-3), Mapping::Continuous);
    }

    #[test]
    fn one_step_is_a_switch() {
        let spec = spec(&described(1, "Bypass", 1), "Bypass".into(), no_names);
        assert_eq!((&spec.info.kind, spec.info.unit.as_str()), (&ParamKind::Bool, ""));
        assert_eq!((spec.info.min, spec.info.max, spec.info.default), (0.0, 1.0, 1.0));
        assert_eq!((spec.mapping.normalize(0.0), spec.mapping.normalize(1.0)), (0.0, 1.0));
        assert_eq!((spec.mapping.plain(0.49), spec.mapping.plain(0.5)), (0.0, 1.0));
    }

    #[test]
    fn a_few_steps_are_choices_when_the_plugin_names_them() {
        let names = |steps: u32| Some((0..=steps).map(|step| format!("Wave {step}")).collect());
        let named = spec(&described(2, "Shape", 3), "Shape".into(), names);
        let choices = vec!["Wave 0".to_string(), "Wave 1".into(), "Wave 2".into(), "Wave 3".into()];
        assert_eq!((&named.info.kind, named.info.unit.as_str()), (&ParamKind::Enum(choices), ""));
        assert_eq!((named.info.min, named.info.max, named.info.default), (0.0, 3.0, 2.0));

        // The value is the index of the choice; normalized, it is that
        // index out of the number of steps.
        let mapping = named.mapping;
        assert_eq!([0.0, 1.0, 2.0, 3.0].map(|plain| mapping.normalize(plain)), [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0]);
        for step in 0..=3 {
            assert_eq!(mapping.plain(mapping.normalize(f64::from(step))), f64::from(step));
        }
        assert_eq!((mapping.normalize(1.6), mapping.normalize(99.0), mapping.normalize(-5.0)), (2.0 / 3.0, 1.0, 0.0));
        // In between, the range is cut into four equal parts.
        assert_eq!(
            [0.0, 0.24, 0.25, 0.5, 0.74, 0.75, 1.0].map(|n| mapping.plain(n)),
            [0.0, 0.0, 1.0, 2.0, 2.0, 3.0, 3.0]
        );

        // Without names the steps are just numbers.
        let unnamed = spec(&described(2, "Shape", 3), "Shape".into(), no_names);
        assert_eq!((&unnamed.info.kind, unnamed.info.unit.as_str()), (&ParamKind::Int, "Hz"));
        assert_eq!(unnamed.mapping, mapping);
    }

    #[test]
    fn many_steps_are_an_integer() {
        let mut asked = false;
        let many = spec(&described(3, "Voices", 65), "Voices".into(), |_| {
            asked = true;
            None
        });
        assert!(!asked, "names are not read for more steps than can be listed");
        assert_eq!((&many.info.kind, many.info.max, many.info.default), (&ParamKind::Int, 65.0, 33.0));
        let most =
            spec(&described(3, "Voices", 64), "Voices".into(), |steps| Some(vec!["x".into(); steps as usize + 1]));
        assert!(matches!(&most.info.kind, ParamKind::Enum(names) if names.len() == 65));

        // Every step survives the round trip, even with very many.
        let mapping = Mapping::of(100_000);
        for step in [0.0, 1.0, 33_333.0, 99_999.0, 100_000.0] {
            assert_eq!(mapping.plain(mapping.normalize(step)), step);
        }
    }

    #[test]
    fn names_are_made_unique_by_unit_and_then_by_number() {
        let in_unit = |id, title: &str, unit| Described { unit, ..described(id, title, 0) };
        let untitled = Described { title: String::new(), short_title: "Vol".into(), ..described(8, "", 0) };
        let nameless = Described { title: String::new(), ..described(9, "", 0) };
        let params = [
            in_unit(1, "Cutoff", 10),
            in_unit(2, "Cutoff", 20),
            in_unit(3, "Cutoff", 20),
            in_unit(4, "Gain", 10),
            in_unit(5, "Mix", kRootUnitId),
            in_unit(6, "Mix", 99),
            in_unit(7, "Mix", 30),
            untitled,
            nameless,
        ];
        let units: HashMap<UnitID, String> =
            [(10, "Filter 1".to_string()), (20, "Filter 2".to_string()), (30, String::new())].into();
        assert_eq!(
            unique_names(&params, &units),
            [
                "Filter 1/Cutoff",
                "Filter 2/Cutoff",
                "Filter 2/Cutoff (2)",
                "Gain", // unambiguous: no unit in front
                "Mix",  // the root unit, an unknown unit and a nameless unit add nothing
                "Mix (2)",
                "Mix (3)",
                "Vol",
                "Parameter 9",
            ]
        );
        assert!(unique_names(&[], &units).is_empty());
    }

    #[test]
    fn units_are_named_by_their_path_below_the_root() {
        let units: HashMap<UnitID, (UnitID, String)> = [
            (kRootUnitId, (-1, "Root".to_string())),
            (1, (kRootUnitId, "Oscillator 1".to_string())),
            (2, (1, "Filter".to_string())),
            (3, (2, String::new())),
            (4, (77, "Orphan".to_string())),
            (5, (6, "Loop A".to_string())),
            (6, (5, "Loop B".to_string())),
        ]
        .into();
        assert_eq!(unit_path(&units, kRootUnitId), "");
        assert_eq!(unit_path(&units, 1), "Oscillator 1");
        assert_eq!(unit_path(&units, 2), "Oscillator 1/Filter");
        assert_eq!(unit_path(&units, 3), "Oscillator 1/Filter", "a nameless unit adds nothing");
        assert_eq!(unit_path(&units, 4), "Orphan");
        assert_eq!(unit_path(&units, 42), "");
        // Units that are each other's parents end somewhere.
        assert_eq!(unit_path(&units, 5).matches("Loop").count(), MAX_UNIT_DEPTH);
    }

    #[test]
    fn model_values_map_to_indices_and_are_clamped() {
        let specs = [
            spec(&described(10, "Level", 0), "Level".into(), no_names),
            spec(&described(20, "Mode", 4), "Mode".into(), no_names),
        ];
        let values = [(20, 100.0), (99, 0.5), (10, 0.25), (10, f64::NAN), (20, 2.6)];
        assert_eq!(resolve_values(&specs, &values), [(1, 4.0), (0, 0.25), (1, 3.0)]);
        assert!(resolve_values(&specs, &[]).is_empty());
    }

    #[test]
    fn a_parameter_is_looked_up_by_channel_and_a_channel_the_plugin_has_not_got_is_its_first() {
        let channel = |number: u8| Channel::new(number).unwrap();
        // Eight channels; a wheel on the first and the sixth, a pedal on
        // the first only.
        let midi = MidiParams::on_channels(8, &[(1, PITCH_BEND, 100), (6, PITCH_BEND, 600), (1, 64, 164)]);
        assert_eq!(midi.param(channel(1), PITCH_BEND), Some(100));
        assert_eq!(midi.param(channel(6), PITCH_BEND), Some(600));
        assert_eq!(midi.param(channel(3), PITCH_BEND), Some(100), "none of its own: the first's");
        assert_eq!(midi.param(channel(6), 64), Some(164));
        assert_eq!(midi.param(channel(6), 11), None);
        assert_eq!(midi.param(channel(6), 200), None, "no such controller");
        // A channel past the bus's last is the first, for notes and for
        // what is set.
        assert_eq!((midi.channel(channel(8)), midi.channel(channel(9))), (channel(8), channel(1)));
        assert_eq!(midi.param(channel(14), PITCH_BEND), Some(100));
        // A plugin that says nothing has one channel and no parameters.
        let plain = MidiParams::default();
        assert_eq!((plain.channel(channel(2)), plain.param(channel(1), 64)), (channel(1), None));
        assert_eq!(MidiParams::plain(40).channel(channel(16)), channel(16));
        assert_eq!(MidiParams::plain(0).channel(channel(2)), channel(1));
    }

    #[test]
    fn the_controllers_a_plugin_names_parameters_for_are_what_it_responds_to() {
        let midi = MidiParams::of(&[(11, 1), (1, 2), (PITCH_BEND, 3), (AFTERTOUCH, 4), (123, 5)]);
        let kinds = vec![
            ControllerKind::MOD_WHEEL,
            ControllerKind::EXPRESSION,
            ControllerKind::SUSTAIN,
            ControllerKind::Pressure,
            ControllerKind::PitchBend,
        ];
        assert_eq!(midi.support(), ControllerSupport::Listed(kinds), "with the pedal, which the host works out");
        assert_eq!(midi.support().responds_to(ControllerKind::BREATH), Some(false));
        assert_eq!(MidiParams::default().support(), ControllerSupport::Listed(vec![ControllerKind::SUSTAIN]));
    }
}
