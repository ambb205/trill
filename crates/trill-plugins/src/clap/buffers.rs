//! A plugin's audio ports as CLAP wants them handed to `process`: the
//! shared [`PortChannels`] and the CLAP buffer structs that point into
//! them.
//!
//! The structs of the outputs are put together by clack. Those of the
//! inputs are filled in here: an input says which of its channels hold
//! one value throughout (silence, here), and clack 0.2 sets the bit of
//! the port's place among the ports where the channel's belongs.

use crate::signal::{PortChannels, all_channels, is_silence};
use clack_host::prelude::*;
use clap_sys::audio_buffer::clap_audio_buffer;
use std::ptr::null_mut;

/// The sample buffers for one direction (input or output) of a plugin.
pub(crate) struct PortBuffers {
    channels: PortChannels,
    /// Scratch space in which clack assembles the CLAP buffer structs of
    /// outputs. Its capacity covers every port and channel, so it never
    /// reallocates.
    ports: AudioPorts,
    /// The CLAP buffer structs of inputs, one a port, and for each port
    /// where each of its channels' samples are. Written anew for every
    /// block, just before the plugin is called.
    raw: Vec<clap_audio_buffer>,
    pointers: Vec<Vec<*mut f32>>,
    /// How many frames a channel holds.
    max_block: usize,
    /// What was loaded into the main port for this block is silence, and
    /// the side port that holds a key in it, if one does.
    main_silent: bool,
    keyed: Option<usize>,
}

// SAFETY: the raw pointers (those in `pointers`, and those to `pointers` in
// `raw`) only ever point into this struct's own heap allocations, and they
// are rewritten by `as_inputs` before each use. Moving the struct to
// another thread moves no allocation and leaves nothing behind that could
// reach them.
unsafe impl Send for PortBuffers {}

impl PortBuffers {
    /// `layout` is the channel count of each port.
    pub(crate) fn new(layout: &[u32], max_block: usize) -> Self {
        let channels = PortChannels::new(layout, max_block);
        let (ports, total) = channels.counts();
        let empty = clap_audio_buffer {
            data32: null_mut(),
            data64: null_mut(),
            channel_count: 0,
            latency: 0,
            constant_mask: 0,
        };
        Self {
            ports: AudioPorts::with_capacity(total, ports),
            channels,
            raw: vec![empty; layout.len()],
            pointers: layout.iter().map(|&count| vec![null_mut(); count as usize]).collect(),
            max_block,
            main_silent: false,
            keyed: None,
        }
    }

    /// Fills the first `frames` frames of every channel with silence.
    pub(crate) fn clear(&mut self, frames: usize) {
        self.channels.clear(frames);
    }

    /// Fills the plugin's inputs for one block from the device's signal.
    pub(crate) fn load(&mut self, left: &[f32], right: &[f32]) {
        self.channels.load(left, right);
        self.main_silent = is_silence(left) && is_silence(right);
        self.keyed = None;
    }

    /// Fills the input `port` for the block that was just loaded from a
    /// key the device listens to.
    pub(crate) fn load_key(&mut self, port: usize, left: &[f32], right: &[f32]) {
        self.channels.load_key(port, left, right);
        self.keyed = Some(port);
    }

    /// Writes the plugin's main output for one block to the device's
    /// signal, on top of what is there if `additive`.
    pub(crate) fn unload(&self, left: &mut [f32], right: &mut [f32], additive: bool) {
        self.channels.unload(left, right, additive);
    }

    /// The first `frames` frames of every port, as CLAP input buffers. A
    /// channel that holds one value throughout is said to be constant,
    /// which here is silence: the main port in a block in which the device
    /// is fed nothing, and every side port the host feeds nothing into, so
    /// that a plugin which rests on silence can.
    pub(crate) fn as_inputs(&mut self, frames: usize) -> InputAudioBuffers<'_> {
        let ports = self.channels.ports_mut().iter_mut();
        for (index, ((raw, pointers), port)) in self.raw.iter_mut().zip(&mut self.pointers).zip(ports).enumerate() {
            for (pointer, channel) in pointers.iter_mut().zip(port.iter_mut()) {
                *pointer = channel.as_mut_ptr();
            }
            let constant = match index {
                0 => self.main_silent,
                _ => self.keyed != Some(index),
            };
            *raw = clap_audio_buffer {
                data32: if pointers.is_empty() { null_mut() } else { pointers.as_mut_ptr() },
                data64: null_mut(),
                channel_count: pointers.len() as u32,
                latency: 0,
                constant_mask: if constant { all_channels(pointers.len()) } else { 0 },
            };
        }
        let frames = frames.min(self.max_block) as u32;
        // SAFETY: every struct points at as many channel pointers as it
        // says, each to `max_block` samples of this struct's own, which is
        // borrowed for as long as what is returned lives; and `frames` is
        // no more than that.
        unsafe { InputAudioBuffers::from_raw_buffers(&self.raw, frames) }
    }

    /// The first `frames` frames of every port, as CLAP output buffers.
    pub(crate) fn as_outputs(&mut self, frames: usize) -> OutputAudioBuffers<'_> {
        self.ports.with_output_buffers(self.channels.ports_mut().iter_mut().map(|port| AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_output_only(port.iter_mut().map(|channel| &mut channel[..frames])),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plugin_without_ports_gets_no_buffers() {
        let mut none = PortBuffers::new(&[], 8);
        none.load(&[1.0; 4], &[1.0; 4]);
        assert_eq!(none.as_inputs(4).port_count(), 0);
        assert_eq!(none.as_outputs(4).frames_count(), None);
    }

    #[test]
    fn the_signal_reaches_the_clap_buffers_and_comes_back() {
        let mut inputs = PortBuffers::new(&[2], 8);
        inputs.load(&[1.0, 2.0, 3.0], &[-1.0, -2.0, -3.0]);
        let raw = inputs.as_inputs(3);
        let raw = raw.as_raw_buffers();
        // SAFETY: clack built these structs over two channels of at least
        // three frames each, which `inputs` keeps alive.
        let (left, right) = unsafe {
            let channels = std::slice::from_raw_parts(raw[0].data32, 2);
            (std::slice::from_raw_parts(channels[0], 3), std::slice::from_raw_parts(channels[1], 3))
        };
        assert_eq!((left, right), (&[1.0, 2.0, 3.0][..], &[-1.0, -2.0, -3.0][..]));

        let mut outputs = PortBuffers::new(&[1], 8);
        outputs.clear(8);
        let (mut left, mut right) = ([0.5f32; 3], [0.5f32; 3]);
        outputs.unload(&mut left, &mut right, true);
        assert_eq!((left, right), ([0.5; 3], [0.5; 3]), "silence added changes nothing");
    }

    #[test]
    fn a_port_that_is_fed_nothing_is_said_to_be_constant_for_that_block_only() {
        let mut inputs = PortBuffers::new(&[2, 2], 8);
        let masks = |inputs: &mut PortBuffers| -> Vec<u64> {
            inputs.as_inputs(4).as_raw_buffers().iter().map(|port| port.constant_mask).collect()
        };
        inputs.load(&[0.0; 4], &[0.0; 4]);
        assert_eq!(masks(&mut inputs), [0b11, 0b11]);
        inputs.load(&[0.0; 4], &[0.0, 0.0, 1e-9, 0.0]);
        assert_eq!(masks(&mut inputs), [0, 0b11], "one sample on one side is sound");
        inputs.load(&[0.0; 4], &[0.0; 4]);
        inputs.load_key(1, &[0.5; 4], &[0.5; 4]);
        assert_eq!(masks(&mut inputs), [0b11, 0], "the key is sound, what it shapes is not");
        // A port of one channel, behind one of two: its own bit and no other.
        let mut inputs = PortBuffers::new(&[2, 1], 8);
        inputs.load(&[0.0; 4], &[0.0; 4]);
        assert_eq!(masks(&mut inputs), [0b11, 0b1]);
        // More frames than a channel holds are not promised.
        assert_eq!(inputs.as_inputs(99).frames_count(), Some(8));
    }

    #[test]
    fn clap_buffers_cover_every_port_for_the_block_only() {
        let mut buffers = PortBuffers::new(&[2, 1], 8);
        let inputs = buffers.as_inputs(5);
        assert_eq!((inputs.port_count(), inputs.frames_count()), (2, Some(5)));
        let channels: Vec<u32> = inputs.port_infos().map(|port| port.channel_count()).collect();
        assert_eq!(channels, [2, 1]);

        let outputs = buffers.as_outputs(3);
        assert_eq!((outputs.port_count(), outputs.frames_count()), (2, Some(3)));
    }

    #[test]
    fn building_clap_buffers_never_reallocates() {
        let mut buffers = PortBuffers::new(&[2, 2, 1], 8);
        // clack fills its scratch space on the first use; from then on its
        // address must not move, or the audio thread has allocated. Nor
        // must that of the inputs' structs, which are filled in here.
        let (inputs, outputs) =
            (buffers.as_inputs(8).as_raw_buffers().as_ptr(), buffers.as_outputs(8).as_raw_buffers().as_ptr());
        for frames in [1, 8, 4] {
            assert_eq!(buffers.as_inputs(frames).as_raw_buffers().as_ptr(), inputs);
            assert_eq!(buffers.as_outputs(frames).as_raw_buffers().as_ptr(), outputs);
        }
    }
}
