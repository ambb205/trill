//! The audio-thread half of a hosted plugin.
//!
//! Everything the plugin needs per block is allocated when the processor
//! is built: its audio buffers, its event queue and the table of pending
//! parameter changes. `process`, `set_param` and `reset` only move data
//! through those; they never allocate, lock or wait.

use super::buffers::PortBuffers;
use super::events::{BlockEvents, ParamFeedback, ParamOutput, transport};
use super::host::{AudioContext, HostShared, TrillHost};
use super::instance::{AudioSlot, Created, InstanceId, PortLayout};
use crate::main_thread::MainThread;
use crate::plugin_thread::PluginThread;
use clack_host::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use trill_core::device::{DeviceProcessor, Event, KeyInput, ProcessInfo};

/// How long to wait for the plugin main thread to tear an instance down
/// before moving on. A stuck plugin must not hang whoever drops a device.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(10);
/// How long re-activating for a new sample rate may take.
const REACTIVATE_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct ClapProcessor {
    /// The plugin's audio processor. It holds `None` only after a failed
    /// re-activation, when the plugin is left inactive and the device
    /// passes audio through.
    audio: Arc<AudioSlot>,
    block: Block,
    main: MainThread<PluginThread>,
    instance: InstanceId,
    plugin_id: String,
    sample_rate: f64,
    latency: u32,
}

/// What a block is processed with, apart from the plugin itself.
struct Block {
    /// Set when the plugin refuses to start or fails a block; it is then
    /// bypassed instead of being asked again on every block.
    failed: bool,
    max_block: usize,
    layout: PortLayout,
    /// Whether the plugin's output is added to the buffer instead of
    /// replacing it. The device API requires this of instruments; it also
    /// applies to anything without an audio input, which could not have
    /// passed the buffer's contents through.
    additive: bool,
    /// The input port a key goes into, if the plugin has one
    /// ([`crate::signal::key_port`]). CLAP has nothing to switch on: the
    /// port is there, and holds silence when nothing is listened to.
    key_port: Option<usize>,
    inputs: PortBuffers,
    outputs: PortBuffers,
    events: BlockEvents,
    /// Where the plugin's own parameter edits are reported.
    feedback: Arc<ParamFeedback>,
    /// Frames processed so far; CLAP wants a clock that never runs back.
    steady_time: u64,
}

impl ClapProcessor {
    /// `instrument` is whether the plugin describes itself as one.
    pub(crate) fn new(
        main: MainThread<PluginThread>,
        plugin_id: String,
        instrument: bool,
        created: Created,
        sample_rate: f64,
        max_block: usize,
    ) -> Self {
        let max_block = max_block.max(1);
        let param_ids = created.params.iter().map(|spec| spec.writable.then_some(spec.id)).collect();
        let mut events = BlockEvents::new(param_ids, created.notes);
        // The model's values were applied on the main thread, before the
        // plugin was activated. Sending them again with the first block
        // costs nothing and covers plugins that reset their parameters when
        // activated, or only honor parameter events while processing.
        for &(index, value) in &created.initial_values {
            events.set_param(index, value);
        }
        Self {
            audio: created.audio,
            block: Block {
                failed: false,
                max_block,
                additive: instrument || created.ports.inputs.is_empty(),
                key_port: crate::signal::key_port(&created.ports.inputs, instrument),
                inputs: PortBuffers::new(&created.ports.inputs, max_block),
                outputs: PortBuffers::new(&created.ports.outputs, max_block),
                layout: created.ports,
                events,
                feedback: created.feedback,
                steady_time: 0,
            },
            main,
            instance: created.id,
            plugin_id,
            sample_rate,
            latency: created.latency,
        }
    }
}

impl Block {
    /// Runs the plugin over the frames `start..start + left.len()` of a
    /// block of `total` frames. `left` and `right` are that part of the
    /// block and no longer than the plugin's maximum block size, and `key`
    /// that part of what the device listens to, if it listens.
    #[allow(clippy::too_many_arguments)]
    fn process_part(
        &mut self,
        audio: &mut PluginAudioProcessor<TrillHost>,
        info: &ProcessInfo,
        events: &[Event],
        (start, total): (usize, usize),
        left: &mut [f32],
        right: &mut [f32],
        key: Option<KeyInput<'_>>,
    ) {
        let frames = left.len();
        // The first block starts the plugin: CLAP wants `start_processing`
        // called from the audio thread.
        let Ok(audio) = audio.ensure_processing_started() else {
            self.failed = true;
            return;
        };
        // Parameter changes travel both ways with every block, which is
        // all a plugin that asked for a flush is waiting for.
        audio.access_shared_handler(HostShared::flush_served);

        let events = self.events.fill(events, start, frames, total);
        self.inputs.load(left, right);
        if let (Some(port), Some(key)) = (self.key_port, key) {
            self.inputs.load_key(port, key.left, key.right);
        }
        // A plugin may return without writing its outputs; what it leaves
        // must not be the previous block.
        self.outputs.clear(frames);

        let result = audio.process(
            &self.inputs.as_inputs(frames),
            &mut self.outputs.as_outputs(frames),
            &InputEvents::from_buffer(events),
            &mut OutputEvents::from_buffer(&mut ParamOutput(&self.feedback)),
            Some(self.steady_time),
            Some(&transport(info, start)),
        );
        self.steady_time += frames as u64;
        match result {
            Ok(_) => self.outputs.unload(left, right, self.additive),
            // The device passes its input through untouched from here on.
            Err(_) => self.failed = true,
        }
    }
}

impl ClapProcessor {
    /// Processes a block, with what the device listens to if it listens.
    fn run(
        &mut self,
        info: &ProcessInfo,
        events: &[Event],
        left: &mut [f32],
        right: &mut [f32],
        key: Option<KeyInput<'_>>,
    ) {
        let total = left.len().min(right.len());
        let block = &mut self.block;
        if total == 0 || block.failed {
            return;
        }
        // The processor is this thread's to use except for the moment in
        // which the main thread flushes a plugin that it took to be idle.
        // A block that falls into such a moment passes its input through,
        // and its notes are kept for the next.
        let processed = self.audio.try_with(|audio| {
            let Some(audio) = audio else { return };
            let _audio_thread = AudioContext::enter();
            // The engine never exceeds `max_block`, the size the plugin was
            // activated for. Should a caller do so anyway, feed the plugin
            // in pieces it can take.
            let mut start = 0;
            while start < total && !block.failed {
                let end = (start + block.max_block).min(total);
                let key = key.map(|key| KeyInput { left: &key.left[start..end], right: &key.right[start..end] });
                let part = (start, total);
                block.process_part(audio, info, events, part, &mut left[start..end], &mut right[start..end], key);
                start = end;
            }
        });
        if processed.is_none() {
            self.block.events.defer(events);
        }
    }
}

impl DeviceProcessor for ClapProcessor {
    /// The factory activates the plugin, so this only has work to do when
    /// the configuration changes: a CLAP plugin must be deactivated and
    /// activated again, on its main thread, to change sample rate.
    fn prepare(&mut self, sample_rate: f64, max_block: usize) {
        let max_block = max_block.max(1);
        if sample_rate == self.sample_rate && max_block == self.block.max_block {
            return;
        }
        let instance = self.instance;
        let frames = u32::try_from(max_block).unwrap_or(u32::MAX);
        let result = self
            .main
            .call_at_rest(REACTIVATE_TIMEOUT, move |thread| thread.clap.reactivate(instance, sample_rate, frames));
        match result {
            Ok(Ok(latency)) => {
                self.latency = latency;
                self.block.failed = false;
            }
            Ok(Err(reason)) => log::error!("[{}] {reason}; the device is bypassed", self.plugin_id),
            Err(error) => log::error!("[{}] cannot re-activate: {error}; the device is bypassed", self.plugin_id),
        }
        self.sample_rate = sample_rate;
        self.block.max_block = max_block;
        self.block.inputs = PortBuffers::new(&self.block.layout.inputs, max_block);
        self.block.outputs = PortBuffers::new(&self.block.layout.outputs, max_block);
        self.block.events.forget_notes();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        self.block.events.set_param(index, value);
    }

    fn process(&mut self, info: &ProcessInfo, events: &[Event], left: &mut [f32], right: &mut [f32]) {
        self.run(info, events, left, right, None);
    }

    fn takes_key(&self) -> bool {
        self.block.key_port.is_some()
    }

    fn process_keyed(
        &mut self,
        info: &ProcessInfo,
        events: &[Event],
        left: &mut [f32],
        right: &mut [f32],
        key: KeyInput<'_>,
    ) {
        self.run(info, events, left, right, Some(key));
    }

    /// CLAP's `reset` is meant to do exactly this, but plugins built on
    /// frameworks that predate CLAP often leave it empty and keep playing.
    /// So the voices this host started are also cut off with the next
    /// block, by means every plugin understands. Tails inside a plugin that
    /// ignores `reset` (a built-in reverb, say) cannot be cleared.
    fn reset(&mut self) {
        self.block.events.silence();
        self.audio.try_with(|audio| {
            if let Some(audio) = audio {
                let _audio_thread = AudioContext::enter();
                audio.reset();
            }
        });
    }

    fn latency_samples(&self) -> u32 {
        self.latency
    }
}

impl Drop for ClapProcessor {
    /// Has the plugin torn down on its main thread, in the order CLAP
    /// requires: stop processing, deactivate, destroy. Processing has
    /// stopped by the fact that this is being dropped.
    ///
    /// This waits for the teardown, so that once a device is dropped its
    /// plugin is really gone, and a DAW shutting down does not exit in the
    /// middle of it. The plugin main thread outlives the factory for as
    /// long as any processor holds a handle to it, so this cannot be left
    /// waiting on a thread that has already stopped.
    fn drop(&mut self) {
        let instance = self.instance;
        if let Err(error) = self.main.call_at_rest(TEARDOWN_TIMEOUT, move |thread| thread.clap.destroy(instance)) {
            log::warn!("[{}] was not torn down cleanly: {error}", self.plugin_id);
        }
    }
}
