//! A plugin's audio ports as CLAP wants them handed to `process`: the
//! shared [`PortChannels`] and the CLAP buffer structs that point into
//! them.

use crate::signal::PortChannels;
use clack_host::prelude::*;

/// The sample buffers for one direction (input or output) of a plugin.
pub(crate) struct PortBuffers {
    channels: PortChannels,
    /// Scratch space in which clack assembles the CLAP buffer structs. Its
    /// capacity covers every port and channel, so it never reallocates.
    ports: AudioPorts,
}

impl PortBuffers {
    /// `layout` is the channel count of each port.
    pub(crate) fn new(layout: &[u32], max_block: usize) -> Self {
        let channels = PortChannels::new(layout, max_block);
        let (ports, total) = channels.counts();
        Self { ports: AudioPorts::with_capacity(total, ports), channels }
    }

    /// Fills the first `frames` frames of every channel with silence.
    pub(crate) fn clear(&mut self, frames: usize) {
        self.channels.clear(frames);
    }

    /// Fills the plugin's inputs for one block from the device's signal.
    pub(crate) fn load(&mut self, left: &[f32], right: &[f32]) {
        self.channels.load(left, right);
    }

    /// Fills the input `port` for the block that was just loaded from a
    /// key the device listens to.
    pub(crate) fn load_key(&mut self, port: usize, left: &[f32], right: &[f32]) {
        self.channels.load_key(port, left, right);
    }

    /// Writes the plugin's main output for one block to the device's
    /// signal, on top of what is there if `additive`.
    pub(crate) fn unload(&self, left: &mut [f32], right: &mut [f32], additive: bool) {
        self.channels.unload(left, right, additive);
    }

    /// The first `frames` frames of every port, as CLAP input buffers.
    pub(crate) fn as_inputs(&mut self, frames: usize) -> InputAudioBuffers<'_> {
        self.ports.with_input_buffers(self.channels.ports_mut().iter_mut().map(|port| AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_input_only(
                port.iter_mut().map(|channel| InputChannel::variable(&mut channel[..frames])),
            ),
        }))
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
        // address must not move, or the audio thread has allocated.
        let first = buffers.as_inputs(8).as_raw_buffers().as_ptr();
        for frames in [1, 8, 4] {
            assert_eq!(buffers.as_inputs(frames).as_raw_buffers().as_ptr(), first);
            assert_eq!(buffers.as_outputs(frames).as_raw_buffers().as_ptr(), first);
        }
    }
}
