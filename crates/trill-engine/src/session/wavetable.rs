//! What the session does for a wavetable synth (`builtin:wavetable`)
//! beyond its parameters: the tables of its own and the names of its
//! macros, which live in the device's state.

use super::{Session, SessionError, readable};
use trill_core::audio::AudioLoader;
use trill_core::{Device, DeviceId};
use trill_dsp::wavetable::factory::{self, Tables};
use trill_dsp::wavetable::recipe::Reading;
use trill_dsp::wavetable::table::FrameInfo;
use trill_dsp::wavetable::{self, State, TableSource};

/// What an oscillator's table is, for whoever chooses or shapes it
/// without hearing it.
#[derive(Clone, Debug, PartialEq)]
pub struct TableReport {
    pub name: String,
    /// Whether it is the device's own, and not one that ships.
    pub own: bool,
    /// What it sounds like and is for, for one that ships.
    pub about: Option<&'static str>,
    /// The file it was read from, and how the file was read.
    pub file: Option<String>,
    pub reading: Option<Reading>,
    /// What each of its frames is like, in order.
    pub frames: Vec<FrameInfo>,
}

impl Session {
    /// The device `id`, if it is a wavetable synth.
    fn wavetable_device(&self, id: DeviceId) -> Result<&Device, SessionError> {
        let device = self.project.device(id)?;
        if device.uri == wavetable::URI {
            Ok(device)
        } else {
            Err(SessionError::Device(format!(
                "{id} ({}) is not a wavetable synth, so it has no tables or macros to set; add a {} device for that",
                device.name,
                wavetable::URI
            )))
        }
    }

    /// What a wavetable synth keeps beside its parameters: its own tables
    /// as they are written down, and the names of its macros.
    pub fn wavetable_state(&self, id: DeviceId) -> Result<State, SessionError> {
        let device = self.wavetable_device(id)?;
        Ok(readable(id, State::from_device_state(device.state.as_deref()))?.unwrap_or_default())
    }

    /// The table oscillator `oscillator` (1 or 2) of a wavetable synth
    /// plays now: the one that ships under the name its parameter has, or
    /// the device's own.
    pub fn wavetable_table(&self, id: DeviceId, oscillator: usize) -> Result<TableReport, SessionError> {
        let which = oscillator_index(oscillator)?;
        let device = self.wavetable_device(id)?;
        let param = table_param(which);
        let chosen = device.params.get(param).copied().unwrap_or(1.0) as usize;
        if let Some(report) = factory_report(chosen) {
            return Ok(report);
        }
        let state = self.wavetable_state(id)?;
        match state.table(which) {
            Some(source) => self.report_of(source).map_err(SessionError::Device),
            None => Err(SessionError::Device(format!(
                "oscillator {oscillator} of {id} is set to a table of its own and the device has none (it plays \
                 '{}' meanwhile): make one for it, or choose one that ships with `{param}`",
                factory::TABLES[0].name
            ))),
        }
    }

    /// What the table `source` describes would be, without giving it to
    /// any device; or why it is none.
    pub fn report_of(&self, source: &TableSource) -> Result<TableReport, String> {
        let media = self.audio.media().clone();
        let loader: &dyn AudioLoader = media.as_ref();
        let (table, reading) = source.make(Some(loader))?;
        Ok(TableReport {
            name: table.name().to_string(),
            own: true,
            about: None,
            file: source.file().map(str::to_string),
            reading,
            frames: table.info().to_vec(),
        })
    }

    /// Gives oscillator `oscillator` (1 or 2) of a wavetable synth a table
    /// of its own, made from a recipe or read from an audio file, and has
    /// the oscillator play it.
    ///
    /// One undo step. The device is made again from its new state, with
    /// its other values as they were; notes that sound are cut. Fails,
    /// changing nothing, if the device is no wavetable synth, the recipe
    /// is wrong or the file is no table, in a sentence that says which.
    pub fn set_wavetable(
        &mut self,
        id: DeviceId,
        oscillator: usize,
        source: TableSource,
    ) -> Result<TableReport, SessionError> {
        let which = oscillator_index(oscillator)?;
        let mut state = self.wavetable_state(id)?;
        let report = self.report_of(&source).map_err(|reason| {
            SessionError::Device(format!("{reason}; oscillator {oscillator} keeps the table it had"))
        })?;
        *state.table_mut(which) = Some(source);
        let bytes = state.to_bytes();
        let revision = self.revision;
        self.edit(|project| {
            let device = project.device_mut(id)?;
            device.state = Some(bytes);
            device.params.insert(table_param(which).to_string(), 0.0);
            Ok(())
        })?;
        self.settle_or_back_out(id, revision)?;
        Ok(report)
    }

    /// Has oscillator `oscillator` (1 or 2) play the table that ships
    /// under `name`. One undo step; the table the device has of its own
    /// stays in its state, to go back to.
    pub fn choose_wavetable(
        &mut self,
        id: DeviceId,
        oscillator: usize,
        name: &str,
    ) -> Result<TableReport, SessionError> {
        let which = oscillator_index(oscillator)?;
        self.wavetable_device(id)?;
        let Some((index, _)) = factory::named(name) else {
            let names: Vec<&str> = factory::TABLES.iter().map(|table| table.name).collect();
            return Err(SessionError::Device(format!(
                "no table ships as '{name}'; the tables are: {}",
                names.join(", ")
            )));
        };
        self.set_device_params(id, &[(table_param(which).to_string(), index as f64)])?;
        Ok(factory_report(index).expect("a table that ships"))
    }

    /// Names macros of a wavetable synth: `names` are `(macro, name)`, the
    /// macro from 1 to 4. An empty name takes a name away. One undo step.
    pub fn name_macros(&mut self, id: DeviceId, names: &[(usize, String)]) -> Result<[String; 4], SessionError> {
        let mut state = self.wavetable_state(id)?;
        for (number, name) in names {
            if !(1..=4).contains(number) {
                return Err(SessionError::Device(format!(
                    "there is no macro {number}; a wavetable synth has macros 1 to 4"
                )));
            }
            let name = name.trim();
            if name.chars().count() > 24 {
                return Err(SessionError::Device(format!("'{name}' is too long for a macro: 24 characters at most")));
            }
            if state.macros.len() < 4 {
                state.macros.resize(4, String::new());
            }
            state.macros[number - 1] = name.to_string();
        }
        while state.macros.last().is_some_and(|name| name.is_empty()) {
            state.macros.pop();
        }
        let named = [0, 1, 2, 3].map(|which| state.macro_name(which));
        let bytes = (!state.is_empty()).then(|| state.to_bytes());
        if self.project.device(id)?.state != bytes {
            let revision = self.revision;
            self.edit(|project| {
                project.device_mut(id)?.state = bytes;
                Ok(())
            })?;
            self.settle_or_back_out(id, revision)?;
        }
        Ok(named)
    }

    /// Takes back the step since `revision` if the device could not be
    /// made from what it was given.
    fn settle_or_back_out(&mut self, id: DeviceId, revision: u64) -> Result<(), SessionError> {
        match self.bank.devices.get(&id).and_then(|meta| meta.error.clone()) {
            Some(reason) => {
                if self.revision != revision {
                    self.back_out();
                }
                Err(SessionError::Device(reason))
            }
            None => Ok(()),
        }
    }
}

/// The parameter that says which table oscillator `which` (0 or 1) plays.
fn table_param(which: usize) -> &'static str {
    ["osc1_table", "osc2_table"][which]
}

fn oscillator_index(oscillator: usize) -> Result<usize, SessionError> {
    match oscillator {
        1 | 2 => Ok(oscillator - 1),
        other => Err(SessionError::Device(format!(
            "there is no oscillator {other}; a wavetable synth has oscillators 1 and 2"
        ))),
    }
}

/// The table that ships as choice `index` of an oscillator's table.
fn factory_report(index: usize) -> Option<TableReport> {
    let made = Tables::get();
    let table = made.chosen(index)?;
    let about = factory::TABLES.get(index - 1).map(|table| table.about);
    Some(TableReport {
        name: table.name().to_string(),
        own: false,
        about,
        file: None,
        reading: None,
        frames: table.info().to_vec(),
    })
}

/// Every table that ships, in the order an oscillator chooses them by.
pub fn factory_tables() -> Vec<TableReport> {
    (1..=factory::TABLES.len()).filter_map(factory_report).collect()
}
