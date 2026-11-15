//! The audio-thread half of a hosted plugin.
//!
//! Everything the plugin needs per block is allocated when the processor
//! is built: its audio buffers, its event queue and the table of pending
//! parameter changes. `process`, `set_param` and `reset` only move data
//! through those; they never allocate, lock or wait.

use super::buffers::PortBuffers;
use super::events::{BlockEvents, transport};
use super::host::{AudioContext, TrillHost};
use super::instance::{Activated, Created, InstanceId, PortLayout};
use crate::main_thread::MainThread;
use crate::plugin_thread::PluginThread;
use clack_host::prelude::*;
use std::time::Duration;
use trill_core::device::{DeviceProcessor, Event, ProcessInfo};

/// How long to wait for the plugin main thread to tear an instance down
/// before moving on. A stuck plugin must not hang whoever drops a device.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(10);
/// How long re-activating for a new sample rate may take.
const REACTIVATE_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct ClapProcessor {
    /// `None` only after a failed re-activation, when the plugin is left
    /// inactive and the device passes audio through.
    audio: Option<PluginAudioProcessor<TrillHost>>,
    /// Set when the plugin refuses to start or fails a block; it is then
    /// bypassed instead of being asked again on every block.
    failed: bool,
    main: MainThread<PluginThread>,
    instance: InstanceId,
    plugin_id: String,

    sample_rate: f64,
    max_block: usize,
    latency: u32,
    layout: PortLayout,
    /// Whether the plugin's output is added to the buffer instead of
    /// replacing it. The device API requires this of instruments; it also
    /// applies to anything without an audio input, which could not have
    /// passed the buffer's contents through.
    additive: bool,
    inputs: PortBuffers,
    outputs: PortBuffers,
    events: BlockEvents,
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
            audio: Some(created.activated.processor.into()),
            failed: false,
            main,
            instance: created.id,
            plugin_id,
            sample_rate,
            max_block,
            latency: created.activated.latency,
            additive: instrument || created.ports.inputs.is_empty(),
            inputs: PortBuffers::new(&created.ports.inputs, max_block),
            outputs: PortBuffers::new(&created.ports.outputs, max_block),
            layout: created.ports,
            events,
            steady_time: 0,
        }
    }

    /// Stops processing, as CLAP requires before deactivating.
    fn stop(&mut self) -> Option<StoppedPluginAudioProcessor<TrillHost>> {
        let audio = self.audio.take()?;
        // `stop_processing` is an audio-thread function. Nothing else is
        // processing this plugin (`&mut self`), so this thread is its audio
        // thread for the duration.
        let _audio_thread = AudioContext::enter();
        Some(audio.into_stopped())
    }

    /// Runs the plugin over the frames `start..start + left.len()` of a
    /// block of `total` frames. `left` and `right` are that part of the
    /// block and no longer than the plugin's maximum block size.
    fn process_part(
        &mut self,
        info: &ProcessInfo,
        events: &[Event],
        (start, total): (usize, usize),
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let frames = left.len();
        // The first block starts the plugin: CLAP wants `start_processing`
        // called from the audio thread.
        let Some(Ok(audio)) = self.audio.as_mut().map(PluginAudioProcessor::ensure_processing_started) else {
            self.failed = true;
            return;
        };

        let events = self.events.fill(events, start, frames, total);
        self.inputs.load(left, right);
        // A plugin may return without writing its outputs; what it leaves
        // must not be the previous block.
        self.outputs.clear(frames);

        let result = audio.process(
            &self.inputs.as_inputs(frames),
            &mut self.outputs.as_outputs(frames),
            &InputEvents::from_buffer(events),
            &mut OutputEvents::void(),
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

impl DeviceProcessor for ClapProcessor {
    /// The factory activates the plugin, so this only has work to do when
    /// the configuration changes: a CLAP plugin must be deactivated and
    /// activated again, on its main thread, to change sample rate.
    fn prepare(&mut self, sample_rate: f64, max_block: usize) {
        let max_block = max_block.max(1);
        if sample_rate == self.sample_rate && max_block == self.max_block {
            return;
        }
        let stopped = self.stop();
        let instance = self.instance;
        let block = u32::try_from(max_block).unwrap_or(u32::MAX);
        let result = self
            .main
            .call(REACTIVATE_TIMEOUT, move |thread| thread.clap.reactivate(instance, stopped, sample_rate, block));
        match result {
            Ok(Ok(Activated { processor, latency })) => {
                self.audio = Some(processor.into());
                self.latency = latency;
                self.failed = false;
            }
            Ok(Err(reason)) => log::error!("[{}] {reason}; the device is bypassed", self.plugin_id),
            Err(error) => log::error!("[{}] cannot re-activate: {error}; the device is bypassed", self.plugin_id),
        }
        self.sample_rate = sample_rate;
        self.max_block = max_block;
        self.inputs = PortBuffers::new(&self.layout.inputs, max_block);
        self.outputs = PortBuffers::new(&self.layout.outputs, max_block);
        self.events.forget_notes();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        self.events.set_param(index, value);
    }

    fn process(&mut self, info: &ProcessInfo, events: &[Event], left: &mut [f32], right: &mut [f32]) {
        let total = left.len().min(right.len());
        if total == 0 || self.failed || self.audio.is_none() {
            return;
        }
        let _audio_thread = AudioContext::enter();
        // The engine never exceeds `max_block`, the size the plugin was
        // activated for. Should a caller do so anyway, feed the plugin in
        // pieces it can take.
        let mut start = 0;
        while start < total && !self.failed {
            let end = (start + self.max_block).min(total);
            self.process_part(info, events, (start, total), &mut left[start..end], &mut right[start..end]);
            start = end;
        }
    }

    /// CLAP's `reset` is meant to do exactly this, but plugins built on
    /// frameworks that predate CLAP often leave it empty and keep playing.
    /// So the voices this host started are also cut off with the next
    /// block, by means every plugin understands. Tails inside a plugin that
    /// ignores `reset` (a built-in reverb, say) cannot be cleared.
    fn reset(&mut self) {
        self.events.silence();
        if let Some(audio) = self.audio.as_mut() {
            let _audio_thread = AudioContext::enter();
            audio.reset();
        }
    }

    fn latency_samples(&self) -> u32 {
        self.latency
    }
}

impl Drop for ClapProcessor {
    /// Tears the plugin down in the order CLAP requires: stop processing
    /// here, then deactivate and destroy on the plugin main thread.
    ///
    /// This waits for the teardown, so that once a device is dropped its
    /// plugin is really gone, and a DAW shutting down does not exit in the
    /// middle of it. The plugin main thread outlives the factory for as
    /// long as any processor holds a handle to it, so this cannot be left
    /// waiting on a thread that has already stopped.
    fn drop(&mut self) {
        let stopped = self.stop();
        let instance = self.instance;
        if let Err(error) = self.main.call(TEARDOWN_TIMEOUT, move |thread| thread.clap.destroy(instance, stopped)) {
            log::warn!("[{}] was not torn down cleanly: {error}", self.plugin_id);
        }
    }
}
