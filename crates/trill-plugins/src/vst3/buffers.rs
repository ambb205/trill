//! A plugin's audio buses as VST3 wants them handed to `process`: the
//! shared [`PortChannels`] and the `AudioBusBuffers` structs that point
//! into them.
//!
//! Every bus the plugin has gets real buffers, whether it was activated or
//! not. Only the main bus carries the device's signal; the others hold
//! silence. Hosts differ on what an inactive bus looks like in `process`
//! (the SDK's own hosting code passes buffers, others pass none), and a
//! plugin that writes to a bus it was told is off must find memory there.

use crate::signal::{PortChannels, all_channels, is_silence};
use std::ptr::null_mut;
use vst3::Steinberg::Vst::{AudioBusBuffers, AudioBusBuffers__type0};
use vst3::Steinberg::int32;

/// Channel count of each audio bus, in bus order. Bus 0 is the main one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BusLayout {
    pub inputs: Vec<u32>,
    pub outputs: Vec<u32>,
}

/// The sample buffers for one direction (input or output) of a plugin.
pub(crate) struct BusBuffers {
    channels: PortChannels,
    /// For each bus, where each of its channels' samples are. Written anew
    /// for every block, just before the plugin is called.
    pointers: Vec<Vec<*mut f32>>,
    /// What `process` is given: one struct per bus, pointing at `pointers`.
    buses: Vec<AudioBusBuffers>,
    /// Whether these are the plugin's inputs, whose side buses the host
    /// declares silent.
    inputs: bool,
    /// The side bus that holds a key in this block, and so is not silent.
    keyed: Option<usize>,
    /// What was loaded into the main bus for this block is silence.
    main_silent: bool,
}

// SAFETY: the raw pointers (those in `pointers`, and those to `pointers` in
// `buses`) only ever point into this struct's own heap allocations, and
// they are rewritten by `prepare` before each use. Moving the struct to
// another thread moves no allocation and leaves nothing behind that could
// reach them.
unsafe impl Send for BusBuffers {}

impl BusBuffers {
    fn new(layout: &[u32], max_block: usize, inputs: bool) -> Self {
        let empty = AudioBusBuffers {
            numChannels: 0,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 { channelBuffers32: null_mut() },
        };
        Self {
            channels: PortChannels::new(layout, max_block),
            pointers: layout.iter().map(|&count| vec![null_mut(); count as usize]).collect(),
            buses: vec![empty; layout.len()],
            inputs,
            keyed: None,
            main_silent: false,
        }
    }

    /// Buffers for a plugin's input buses; `layout` is the channel count
    /// of each.
    pub(crate) fn inputs(layout: &[u32], max_block: usize) -> Self {
        Self::new(layout, max_block, true)
    }

    /// Buffers for a plugin's output buses.
    pub(crate) fn outputs(layout: &[u32], max_block: usize) -> Self {
        Self::new(layout, max_block, false)
    }

    /// How many buses there are.
    pub(crate) fn count(&self) -> int32 {
        self.buses.len() as int32
    }

    /// Fills the first `frames` frames of every channel with silence.
    pub(crate) fn clear(&mut self, frames: usize) {
        self.channels.clear(frames);
    }

    /// Fills the plugin's inputs for one block from the device's signal.
    pub(crate) fn load(&mut self, left: &[f32], right: &[f32]) {
        self.channels.load(left, right);
        self.keyed = None;
        self.main_silent = is_silence(left) && is_silence(right);
    }

    /// Fills the input bus `bus` for the block that was just loaded from a
    /// key the device listens to.
    pub(crate) fn load_key(&mut self, bus: usize, left: &[f32], right: &[f32]) {
        self.channels.load_key(bus, left, right);
        self.keyed = Some(bus);
    }

    /// Writes the plugin's main output for one block to the device's
    /// signal, on top of what is there if `additive`.
    pub(crate) fn unload(&self, left: &mut [f32], right: &mut [f32], additive: bool) {
        self.channels.unload(left, right, additive);
    }

    /// The buses as `process` takes them, or null if there are none.
    ///
    /// The result points into this struct. It is valid until the buffers
    /// are next touched through any other method, which is why this is
    /// called last before `process`, and again for every block.
    pub(crate) fn prepare(&mut self) -> *mut AudioBusBuffers {
        let ports = self.channels.ports_mut().iter_mut();
        for (index, ((bus, pointers), port)) in self.buses.iter_mut().zip(&mut self.pointers).zip(ports).enumerate() {
            for (pointer, channel) in pointers.iter_mut().zip(port.iter_mut()) {
                *pointer = channel.as_mut_ptr();
            }
            bus.numChannels = pointers.len() as int32;
            // One bit per channel that holds nothing but silence: all of a
            // side bus the host feeds nothing into, and all of the main
            // bus in a block in which the device is fed nothing, so that a
            // plugin which rests on silence can. A plugin sets the bits of
            // its outputs itself.
            let silent = match index {
                0 => self.inputs && self.main_silent,
                _ => self.inputs && self.keyed != Some(index),
            };
            bus.silenceFlags = if silent { all_channels(pointers.len()) } else { 0 };
            let channels = if pointers.is_empty() { null_mut() } else { pointers.as_mut_ptr() };
            bus.__field0 = AudioBusBuffers__type0 { channelBuffers32: channels };
        }
        if self.buses.is_empty() { null_mut() } else { self.buses.as_mut_ptr() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a plugin would find in the buses `prepare` returned: for each
    /// bus, its silence flags and the first `frames` samples of each
    /// channel.
    fn read(buses: *mut AudioBusBuffers, count: int32, frames: usize) -> Vec<(u64, Vec<Vec<f32>>)> {
        (0..count as usize)
            .map(|index| {
                // SAFETY: `prepare` returned `count` bus structs, each
                // with `numChannels` channel pointers to at least `frames`
                // samples, all owned by the `BusBuffers` under test.
                unsafe {
                    let bus = &*buses.add(index);
                    let channels = (0..bus.numChannels as usize)
                        .map(|channel| {
                            let samples = *bus.__field0.channelBuffers32.add(channel);
                            std::slice::from_raw_parts(samples, frames).to_vec()
                        })
                        .collect();
                    (bus.silenceFlags, channels)
                }
            })
            .collect()
    }

    #[test]
    fn the_signal_reaches_the_main_bus_and_side_buses_are_flagged_silent() {
        let mut inputs = BusBuffers::inputs(&[2, 2, 1, 0], 8);
        inputs.load(&[1.0, 2.0, 3.0], &[-1.0, -2.0, -3.0]);
        assert_eq!(inputs.count(), 4);
        let buses = inputs.prepare();
        assert_eq!(
            read(buses, 4, 3),
            [
                (0, vec![vec![1.0, 2.0, 3.0], vec![-1.0, -2.0, -3.0]]),
                (0b11, vec![vec![0.0; 3]; 2]),
                (0b1, vec![vec![0.0; 3]]),
                (0, vec![]),
            ]
        );
        // SAFETY: `buses` has four entries; the last has no channels.
        assert!(unsafe { (*buses.add(3)).__field0.channelBuffers32 }.is_null());
    }

    #[test]
    fn a_main_bus_that_is_fed_nothing_is_flagged_silent_for_that_block_only() {
        let mut inputs = BusBuffers::inputs(&[2, 2], 8);
        inputs.load(&[0.0; 4], &[0.0, -0.0, 0.0, 0.0]);
        let flags = |buses: *mut AudioBusBuffers| -> Vec<u64> { read(buses, 2, 4).iter().map(|bus| bus.0).collect() };
        let buses = inputs.prepare();
        assert_eq!(flags(buses), [0b11, 0b11]);
        // One sample on one side is sound.
        inputs.load(&[0.0; 4], &[0.0, 0.0, 1e-9, 0.0]);
        let buses = inputs.prepare();
        assert_eq!(flags(buses), [0, 0b11]);
        inputs.load(&[0.0; 4], &[0.0; 4]);
        inputs.load_key(1, &[0.5; 4], &[0.5; 4]);
        let buses = inputs.prepare();
        assert_eq!(flags(buses), [0b11, 0], "the key is sound, what it shapes is not");
        // What a plugin gives is the plugin's to flag.
        let mut outputs = BusBuffers::outputs(&[2], 8);
        outputs.load(&[0.0; 4], &[0.0; 4]);
        assert_eq!(read(outputs.prepare(), 1, 4)[0].0, 0);
    }

    #[test]
    fn what_the_plugin_writes_comes_back_as_the_devices_signal() {
        let mut outputs = BusBuffers::outputs(&[2, 2], 8);
        outputs.clear(8);
        let buses = outputs.prepare();
        assert_eq!(read(buses, 2, 2), [(0, vec![vec![0.0; 2]; 2]), (0, vec![vec![0.0; 2]; 2])]);
        // SAFETY: the plugin's part: write two frames to each channel of
        // both buses, through the pointers `prepare` handed out.
        unsafe {
            for (bus, value) in [(0, 0.5), (1, 9.0)] {
                for channel in 0..2 {
                    let samples = *(*buses.add(bus)).__field0.channelBuffers32.add(channel);
                    std::slice::from_raw_parts_mut(samples, 2).fill(value * (channel + 1) as f32);
                }
            }
        }
        let (mut left, mut right) = ([1.0f32; 2], [1.0f32; 2]);
        outputs.unload(&mut left, &mut right, true);
        assert_eq!((left, right), ([1.5; 2], [2.0; 2]), "the main bus is added; the other is ignored");
    }

    #[test]
    fn a_plugin_without_buses_gets_none() {
        let mut none = BusBuffers::inputs(&[], 8);
        none.load(&[1.0; 4], &[1.0; 4]);
        assert_eq!(none.count(), 0);
        assert!(none.prepare().is_null());
    }

    #[test]
    fn preparing_a_block_never_reallocates() {
        let mut buffers = BusBuffers::outputs(&[2, 1], 8);
        let first = buffers.prepare();
        let channels = read(first, 2, 1).len();
        for _ in 0..3 {
            buffers.clear(8);
            assert_eq!(buffers.prepare(), first);
        }
        assert_eq!(channels, 2);
    }

    #[test]
    fn silence_flags_cover_exactly_the_channels() {
        assert_eq!(all_channels(0), 0);
        assert_eq!(all_channels(1), 0b1);
        assert_eq!(all_channels(6), 0b11_1111);
        assert_eq!(all_channels(64), u64::MAX);
        assert_eq!(all_channels(200), u64::MAX);
    }
}
