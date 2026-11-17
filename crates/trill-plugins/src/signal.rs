//! Host-owned sample buffers for a plugin's audio ports, and the mapping
//! between them and the device API's fixed stereo signal.
//!
//! A device is always handed one stereo pair. A plugin has any number of
//! ports (VST3 calls them buses) with any number of channels each, or none.
//! Only the first port in each direction, the "main" one, carries the
//! device's signal: stereo as it is, mono folded down on the way in and
//! duplicated on the way out, and anything wider on its first two channels.
//! One more input can carry a key, the signal of another track that the
//! device listens to and does not pass on ([`key_port`]): the same way.
//! Further ports, such as auxiliary outputs, get silence and are ignored.
//!
//! Each plugin format wraps these buffers in the structures its API passes
//! to `process`.

/// The sample buffers for one direction (input or output) of a plugin.
///
/// Inputs and outputs are always separate buffers: neither CLAP nor VST3
/// lets a host assume a plugin can process in place, and copying two
/// channels per block is cheaper than finding out which can.
pub(crate) struct PortChannels {
    /// `channels[port][channel]`, each as long as the largest block.
    channels: Vec<Vec<Vec<f32>>>,
}

impl PortChannels {
    /// `layout` is the channel count of each port.
    pub(crate) fn new(layout: &[u32], max_block: usize) -> Self {
        Self { channels: layout.iter().map(|&count| vec![vec![0.0; max_block]; count as usize]).collect() }
    }

    /// The channels of every port, in port order. The buffers must not be
    /// resized: plugins are handed pointers to them.
    pub(crate) fn ports_mut(&mut self) -> &mut [Vec<Vec<f32>>] {
        &mut self.channels
    }

    /// How many ports there are, and how many channels in all of them.
    pub(crate) fn counts(&self) -> (usize, usize) {
        (self.channels.len(), self.channels.iter().map(Vec::len).sum())
    }

    fn main(&self) -> &[Vec<f32>] {
        self.channels.first().map_or(&[], Vec::as_slice)
    }

    /// Fills the first `frames` frames of every channel with silence.
    pub(crate) fn clear(&mut self, frames: usize) {
        for channel in self.channels.iter_mut().flatten() {
            channel[..frames].fill(0.0);
        }
    }

    /// Fills the plugin's inputs for one block from the device's signal.
    /// `left` and `right` have the block's length.
    pub(crate) fn load(&mut self, left: &[f32], right: &[f32]) {
        self.clear(left.len());
        self.fill(0, left, right);
    }

    /// Fills the input `port` for the block that was just
    /// [loaded](Self::load) from a key the device listens to.
    pub(crate) fn load_key(&mut self, port: usize, left: &[f32], right: &[f32]) {
        self.fill(port, left, right);
    }

    /// Puts a stereo signal into `port`: as it is, folded down to mono, or
    /// on the first two of more channels.
    fn fill(&mut self, port: usize, left: &[f32], right: &[f32]) {
        let frames = left.len();
        match self.channels.get_mut(port).map_or(&mut [][..], Vec::as_mut_slice) {
            [] => {}
            [mono] => {
                for ((sample, left), right) in mono.iter_mut().zip(left).zip(right) {
                    *sample = (left + right) * 0.5;
                }
            }
            [first, second, ..] => {
                first[..frames].copy_from_slice(left);
                second[..frames].copy_from_slice(right);
            }
        }
    }

    /// Writes the plugin's main output for one block to the device's
    /// signal, on top of what is there if `additive`. A plugin without an
    /// output leaves the signal alone.
    ///
    /// Third-party DSP is where NaNs come from, and a single one poisons
    /// every filter and delay line downstream for good, so samples that are
    /// not finite are replaced with silence here.
    pub(crate) fn unload(&self, left: &mut [f32], right: &mut [f32], additive: bool) {
        let (out_left, out_right) = match self.main() {
            [] => return,
            [mono] => (mono, mono),
            [first, second, ..] => (first, second),
        };
        for (signal, output) in [(left, out_left), (right, out_right)] {
            for (sample, out) in signal.iter_mut().zip(output) {
                let out = if out.is_finite() { *out } else { 0.0 };
                *sample = if additive { *sample + out } else { out };
            }
        }
    }
}

/// The input of a plugin that a key goes into, if it has one: `inputs` is
/// the channel count of each of its input ports.
///
/// An effect's first input is its own signal, and the one after it is
/// what both formats have for a sidechain. An instrument has no signal of
/// its own to take in, so an input it has is one to listen to: a vocoder's
/// voice.
pub(crate) fn key_port(inputs: &[u32], instrument: bool) -> Option<usize> {
    let port = usize::from(!instrument);
    inputs.get(port).is_some_and(|channels| *channels > 0).then_some(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_goes_into_the_input_after_an_effects_own_and_into_an_instruments_first() {
        assert_eq!(key_port(&[2, 1], false), Some(1));
        assert_eq!(key_port(&[2], false), None, "an effect with one input listens to that");
        assert_eq!(key_port(&[2, 0], false), None, "an input without channels takes nothing");
        assert_eq!(key_port(&[2, 2, 2], false), Some(1), "the first of several");
        assert_eq!(key_port(&[2], true), Some(0));
        assert_eq!(key_port(&[], true), None);

        // The key is laid into its port as the signal is into the first:
        // folded down for a mono port, and beside the signal, not over it.
        let mut buffers = PortChannels::new(&[2, 1, 2], 8);
        buffers.load(&LEFT, &RIGHT);
        buffers.load_key(1, &[1.0, 1.0, 0.0, 0.0], &[0.0, 1.0, 0.0, -2.0]);
        assert_eq!(buffers.channels[0][0][..4], LEFT);
        assert_eq!(buffers.channels[0][1][..4], RIGHT);
        assert_eq!(buffers.channels[1][0][..4], [0.5, 1.0, 0.0, -1.0]);
        assert_eq!(buffers.channels[2][0][..4], [0.0; 4], "a port beyond it stays silent");
        // The next block without a key has none left in it.
        buffers.load(&LEFT, &RIGHT);
        assert_eq!(buffers.channels[1][0][..4], [0.0; 4]);
    }

    const LEFT: [f32; 4] = [1.0, 2.0, 3.0, 4.0];
    const RIGHT: [f32; 4] = [-1.0, 0.0, 1.0, 2.0];

    /// Buffers for `layout` with every sample set to a recognizable value,
    /// standing in for whatever the previous block left behind.
    fn stale(layout: &[u32]) -> PortChannels {
        let mut buffers = PortChannels::new(layout, 8);
        for channel in buffers.channels.iter_mut().flatten() {
            channel.fill(9.0);
        }
        buffers
    }

    #[test]
    fn the_layout_decides_the_buffers() {
        let mut buffers = PortChannels::new(&[2, 0, 1], 8);
        assert_eq!(buffers.counts(), (3, 3));
        let lengths: Vec<Vec<usize>> =
            buffers.ports_mut().iter().map(|port| port.iter().map(Vec::len).collect()).collect();
        assert_eq!(lengths, [vec![8, 8], vec![], vec![8]]);
        assert_eq!(PortChannels::new(&[], 8).counts(), (0, 0));
    }

    #[test]
    fn a_stereo_input_takes_the_signal_as_it_is() {
        let mut buffers = stale(&[2]);
        buffers.load(&LEFT, &RIGHT);
        assert_eq!(buffers.channels[0][0][..4], LEFT);
        assert_eq!(buffers.channels[0][1][..4], RIGHT);
    }

    #[test]
    fn a_mono_input_gets_the_average_of_both_sides() {
        let mut buffers = stale(&[1]);
        buffers.load(&LEFT, &RIGHT);
        assert_eq!(buffers.channels[0][0][..4], [0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn extra_channels_and_side_chains_get_silence() {
        let mut buffers = stale(&[4, 2, 0]);
        buffers.load(&LEFT, &RIGHT);
        assert_eq!(buffers.channels[0][0][..4], LEFT);
        assert_eq!(buffers.channels[0][1][..4], RIGHT);
        for channel in buffers.channels[0][2..].iter().chain(&buffers.channels[1]) {
            assert_eq!(channel[..4], [0.0; 4]);
        }
        assert!(buffers.channels[2].is_empty());
    }

    #[test]
    fn a_plugin_without_inputs_or_outputs_is_left_out() {
        let mut none = PortChannels::new(&[], 8);
        none.load(&LEFT, &RIGHT);
        let (mut left, mut right) = (LEFT, RIGHT);
        none.unload(&mut left, &mut right, false);
        assert_eq!((left, right), (LEFT, RIGHT), "no output port: the signal passes through");

        // A main port that exists but has no channels is no different.
        let mut empty = PortChannels::new(&[0], 8);
        empty.load(&LEFT, &RIGHT);
        empty.unload(&mut left, &mut right, false);
        assert_eq!((left, right), (LEFT, RIGHT));
    }

    #[test]
    fn output_replaces_or_adds() {
        let mut buffers = PortChannels::new(&[2, 2], 8);
        buffers.channels[0][0][..4].copy_from_slice(&[0.5; 4]);
        buffers.channels[0][1][..4].copy_from_slice(&[0.25; 4]);
        buffers.channels[1][0].fill(7.0); // an auxiliary output: ignored

        let (mut left, mut right) = (LEFT, RIGHT);
        buffers.unload(&mut left, &mut right, false);
        assert_eq!((left, right), ([0.5; 4], [0.25; 4]));

        let (mut left, mut right) = (LEFT, RIGHT);
        buffers.unload(&mut left, &mut right, true);
        assert_eq!(left, [1.5, 2.5, 3.5, 4.5]);
        assert_eq!(right, [-0.75, 0.25, 1.25, 2.25]);
    }

    #[test]
    fn a_mono_output_goes_to_both_sides() {
        let mut buffers = PortChannels::new(&[1], 8);
        buffers.channels[0][0][..4].copy_from_slice(&[0.1, 0.2, 0.3, 0.4]);
        let (mut left, mut right) = ([0.0; 4], [1.0; 4]);
        buffers.unload(&mut left, &mut right, false);
        assert_eq!(left, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(right, left);
    }

    #[test]
    fn output_that_is_not_a_number_becomes_silence() {
        let mut buffers = PortChannels::new(&[2], 8);
        buffers.channels[0][0][..4].copy_from_slice(&[f32::NAN, 0.5, f32::INFINITY, f32::NEG_INFINITY]);
        buffers.channels[0][1][..4].copy_from_slice(&[0.5; 4]);
        let (mut left, mut right) = ([1.0; 4], [1.0; 4]);
        buffers.unload(&mut left, &mut right, true);
        assert_eq!(left, [1.0, 1.5, 1.0, 1.0]);
        assert_eq!(right, [1.5; 4]);
    }
}
