//! Static parameter tables for built-in devices.

use daw_core::device::{ParamInfo, ParamKind, Preset};

#[derive(Copy, Clone, Debug)]
pub enum Kind {
    Float,
    Int,
    Bool,
    Enum(&'static [&'static str]),
}

#[derive(Copy, Clone, Debug)]
pub struct ParamDef {
    pub id: &'static str,
    pub name: &'static str,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub unit: &'static str,
    pub kind: Kind,
}

pub const fn float(
    id: &'static str,
    name: &'static str,
    min: f64,
    max: f64,
    default: f64,
    unit: &'static str,
) -> ParamDef {
    ParamDef { id, name, min, max, default, unit, kind: Kind::Float }
}

pub const fn int(
    id: &'static str,
    name: &'static str,
    min: i32,
    max: i32,
    default: i32,
    unit: &'static str,
) -> ParamDef {
    ParamDef { id, name, min: min as f64, max: max as f64, default: default as f64, unit, kind: Kind::Int }
}

pub const fn boolean(id: &'static str, name: &'static str, default: bool) -> ParamDef {
    ParamDef { id, name, min: 0.0, max: 1.0, default: if default { 1.0 } else { 0.0 }, unit: "", kind: Kind::Bool }
}

pub const fn choice(
    id: &'static str,
    name: &'static str,
    choices: &'static [&'static str],
    default: usize,
) -> ParamDef {
    ParamDef {
        id,
        name,
        min: 0.0,
        max: (choices.len() - 1) as f64,
        default: default as f64,
        unit: "",
        kind: Kind::Enum(choices),
    }
}

impl ParamDef {
    pub fn info(&self) -> ParamInfo {
        ParamInfo {
            id: self.id.to_string(),
            name: self.name.to_string(),
            min: self.min,
            max: self.max,
            default: self.default,
            unit: self.unit.to_string(),
            kind: match self.kind {
                Kind::Float => ParamKind::Float,
                Kind::Int => ParamKind::Int,
                Kind::Bool => ParamKind::Bool,
                Kind::Enum(choices) => ParamKind::Enum(choices.iter().map(|c| c.to_string()).collect()),
            },
        }
    }

    fn clamp(&self, value: f64) -> f64 {
        let v = if value.is_finite() { value } else { self.default };
        let v = v.clamp(self.min, self.max);
        match self.kind {
            Kind::Float => v,
            _ => v.round(),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct PresetDef {
    pub name: &'static str,
    pub description: &'static str,
    pub params: &'static [(&'static str, f64)],
}

impl PresetDef {
    pub fn preset(&self) -> Preset {
        Preset {
            name: self.name.to_string(),
            description: self.description.to_string(),
            params: self.params.iter().map(|(id, value)| (id.to_string(), *value)).collect(),
        }
    }
}

/// Current values of a device's parameters, indexed like its table.
pub struct ParamValues {
    defs: &'static [ParamDef],
    values: Box<[f64]>,
    /// Set whenever a value changes; the device clears it after refreshing
    /// whatever it derives from its parameters.
    pub changed: bool,
}

impl ParamValues {
    pub fn new(defs: &'static [ParamDef]) -> Self {
        Self { defs, values: defs.iter().map(|d| d.default).collect(), changed: true }
    }

    pub fn set(&mut self, index: usize, value: f64) {
        if let Some(def) = self.defs.get(index) {
            let value = def.clamp(value);
            if self.values[index] != value {
                self.values[index] = value;
                self.changed = true;
            }
        }
    }

    #[inline]
    pub fn get(&self, index: usize) -> f32 {
        self.values[index] as f32
    }

    #[inline]
    pub fn on(&self, index: usize) -> bool {
        self.values[index] >= 0.5
    }

    #[inline]
    pub fn index(&self, index: usize) -> usize {
        self.values[index] as usize
    }
}

/// Declares a parameter table together with an enum of indices into it, so
/// the two cannot drift apart.
#[macro_export]
macro_rules! define_params {
    ($index:ident, $table:ident, [ $( $variant:ident => $def:expr ),* $(,)? ]) => {
        #[derive(Copy, Clone)]
        #[repr(usize)]
        enum $index { $($variant),* }

        pub const $table: &[$crate::params::ParamDef] = &[ $($def),* ];
    };
}
