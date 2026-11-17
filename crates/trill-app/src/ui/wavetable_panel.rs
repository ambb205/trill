//! What the wavetable synth's panel shows that no other device has: the
//! wave an oscillator reads at its position, and its macros under the
//! names the sound gives them.
//!
//! A table that ships is there to be read as soon as a synth exists: the
//! engine made them all when it created the device. A table of the
//! device's own is made again here, for the picture, on a thread of its
//! own: nothing that reads a file or takes a moment runs on this one.

use super::devices::Body;
use super::state::Cx;
use eframe::egui::{Context, Id, Rect, Role, Sense, Ui, WidgetInfo};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use trill_core::Device;
use trill_core::audio::AudioLoader;
use trill_dsp::wavetable::factory::Tables;
use trill_dsp::wavetable::table::Table;
use trill_dsp::wavetable::{State, TableSource};
use trill_noir::Plot;

/// The parameters that say which table an oscillator plays and where in
/// it, by oscillator.
const TABLE: [&str; 2] = ["osc1_table", "osc2_table"];
const POSITION: [&str; 2] = ["osc1_position", "osc2_position"];
/// Points a wave is drawn with: more than a display is wide.
const POINTS: usize = 192;

/// A table of a device's own, as it is being made for the picture: the
/// state it was made from, and the table once there is one.
#[derive(Clone)]
struct Own {
    made_from: u64,
    table: Arc<Mutex<Option<Arc<Table>>>>,
}

/// The device's state as a number that changes when it does.
fn fingerprint(device: &Device, oscillator: usize) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    device.state.hash(&mut hasher);
    oscillator.hash(&mut hasher);
    hasher.finish()
}

/// The table oscillator `oscillator` (0 or 1) of `device` has of its own,
/// if it has one and it has been made; making it is started here the
/// first time it is asked for, and the window is drawn again when it is
/// there.
fn own_table(ui: &Ui, cx: &Cx, memory: Id, device: &Device, oscillator: usize) -> Option<Arc<Table>> {
    let made_from = fingerprint(device, oscillator);
    let kept: Option<Own> = ui.data(|data| data.get_temp(memory));
    if let Some(own) = kept.filter(|own| own.made_from == made_from) {
        return own.table.lock().ok()?.clone();
    }
    let state = State::from_device_state(device.state.as_deref()).ok()?;
    let source: TableSource = state.table(oscillator)?.clone();
    let own = Own { made_from, table: Arc::new(Mutex::new(None)) };
    ui.data_mut(|data| data.insert_temp(memory, own.clone()));
    let media = cx.session.lock().media().clone();
    let context: Context = ui.ctx().clone();
    std::thread::spawn(move || {
        let loader: &dyn AudioLoader = media.as_ref();
        if let Ok((table, _)) = source.make(Some(loader)) {
            if let Ok(mut slot) = own.table.lock() {
                *slot = Some(Arc::new(table));
            }
            context.request_repaint();
        }
    });
    None
}

/// The wave oscillator `oscillator` (0 or 1) reads now, with what the
/// table is called and how many frames it has.
fn wave(ui: &Ui, cx: &Cx, body: &Body, oscillator: usize) -> (Vec<f32>, String, usize) {
    let chosen = body.param(TABLE[oscillator]).map_or(1.0, |param| param.value).round().max(0.0) as usize;
    let position = body.param(POSITION[oscillator]).map_or(0.5, |param| param.value) as f32;
    let tables = Tables::get();
    match tables.chosen(chosen) {
        Some(table) => (table.outline(position, POINTS), table.name().to_string(), table.frames()),
        None => match own_table(ui, cx, body.id.with(("own table", oscillator)), body.device, oscillator) {
            Some(table) => (table.outline(position, POINTS), table.name().to_string(), table.frames()),
            None => (vec![0.0; POINTS], "its own table".to_string(), 0),
        },
    }
}

/// Paints the wave oscillator `oscillator` (0 or 1) reads at its position
/// into `rect`, with a mark along the bottom for where in the table that
/// is.
pub fn table(ui: &Ui, cx: &mut Cx, body: &Body, rect: Rect, oscillator: usize) {
    let (wave, name, frames) = wave(ui, cx, body, oscillator);
    let position = body.param(POSITION[oscillator]).map_or(0.5, |param| param.value) as f32;
    let accent = if body.dimmed { trill_noir::fade(body.accent, cx.theme.disabled) } else { body.accent };
    let plot = Plot::new(ui.painter(), cx.theme, rect);
    plot.horizontals(&[0.5]);
    plot.wave(accent, &wave);
    plot.position(cx.theme.ink, position);
    let response = ui.interact(rect, body.id.with(("table", oscillator)), Sense::hover());
    let said = match frames {
        0 => format!("Oscillator {} reads {name}", oscillator + 1),
        1 => format!("Oscillator {} reads {name}: one frame", oscillator + 1),
        frames => format!("Oscillator {} reads {name}: {frames} frames, here at {position:.2}", oscillator + 1),
    };
    response.widget_info(|| WidgetInfo::labeled(Role::Image, true, &said));
    cx.hint(&response, said.clone());
}

/// What macro `which` (0..4) of `device` is called.
pub fn macro_name(device: &Device, which: usize) -> String {
    match State::from_device_state(device.state.as_deref()) {
        Ok(state) => state.macro_name(which),
        Err(_) => format!("Macro {}", which + 1),
    }
}
