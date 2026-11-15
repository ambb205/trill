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
use std::collections::{HashMap, HashSet};
use trill_core::device::{ParamInfo, ParamKind};
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
    }
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

/// The parameters through which the plugin takes MIDI's "all sound off"
/// and "all notes off".
///
/// A VST3 plugin is not sent MIDI controllers. One that understands them
/// names a parameter for each, and the host sets that parameter instead.
/// These two are how a host asks a plugin to fall silent.
pub(crate) fn silencing_params(controller: &ComPtr<IEditController>) -> Vec<ParamID> {
    let Some(mapping) = controller.cast::<IMidiMapping>() else { return Vec::new() };
    [kCtrlAllSoundsOff, kCtrlAllNotesOff]
        .into_iter()
        .filter_map(|controller_number| {
            let mut id: ParamID = 0;
            // SAFETY: `mapping` is an interface of the live edit
            // controller, used on the plugin main thread; `id` is a local.
            let result = unsafe { mapping.getMidiControllerAssignment(0, 0, controller_number as CtrlNumber, &mut id) };
            (result == kResultOk).then_some(id)
        })
        .collect()
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
}
