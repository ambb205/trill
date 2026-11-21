//! The audio-thread half of a hosted plugin.
//!
//! Everything the plugin needs per block is allocated when the processor
//! is built: its audio buffers, its event list and its parameter-change
//! lists. `process`, `set_param` and `reset` only move data through those;
//! they never allocate, lock or wait.

use super::buffers::{BusBuffers, BusLayout};
use super::changes::BlockParams;
use super::com::interface;
use super::events::{BlockEvents, EventList, process_context};
use super::instance::{Created, InstanceId};
use crate::main_thread::MainThread;
use crate::plugin_thread::PluginThread;
use std::ptr::null_mut;
use std::time::Duration;
use trill_core::device::{DeviceProcessor, Event, KeyInput, ProcessInfo};
use vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32;
use vst3::Steinberg::Vst::{IAudioProcessor, IAudioProcessorTrait, ProcessData};
use vst3::Steinberg::{int32, kResultOk};
use vst3::{ComPtr, ComWrapper};

/// How long to wait for the plugin main thread to tear an instance down
/// before moving on. A stuck plugin must not hang whoever drops a device.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(10);
/// How long re-activating for a new sample rate may take.
const REACTIVATE_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct Vst3Processor {
    /// The plugin's processing interface. `None` only once the processor
    /// is being dropped and has handed it back to the plugin main thread.
    audio: Option<ComPtr<IAudioProcessor>>,
    /// False after a failed re-activation, when the plugin is left
    /// inactive and the device passes audio through.
    active: bool,
    /// Whether the plugin has been told that processing has started.
    processing: bool,
    /// Set when the plugin fails a block; it is then bypassed instead of
    /// being asked again on every block.
    failed: bool,
    main: MainThread<PluginThread>,
    instance: InstanceId,
    /// The plugin's name, for messages.
    name: String,

    sample_rate: f64,
    max_block: usize,
    latency: u32,
    layout: BusLayout,
    /// Whether the plugin's output is added to the buffer instead of
    /// replacing it. The device API requires this of instruments; it also
    /// applies to anything without an audio input, which could not have
    /// passed the buffer's contents through.
    additive: bool,
    /// The input bus a key goes into, if the plugin has one
    /// ([`crate::signal::key_port`]), and whether it is switched on. An
    /// instrument's is its first bus, which always is. An effect's is
    /// switched on when the plugin is made, for a device that listens to
    /// another track, and not otherwise: so the plugin is made again when
    /// that changes.
    key_bus: Option<usize>,
    key_on: bool,
    inputs: BusBuffers,
    outputs: BusBuffers,
    events: BlockEvents,
    /// Where the plugin puts events of its own. Nothing reads them, but
    /// plugins expect the list to be there.
    output_events: ComWrapper<EventList>,
    params: BlockParams,
    /// Frames processed so far: VST3's clock that never runs back.
    continuous_samples: i64,
    /// The plugin was set up for offline processing (a render).
    offline: bool,
    /// Whether the plugin is given the time it is known to need before it
    /// is taken down ([`super::quirks`]).
    patient: bool,
}

impl Vst3Processor {
    /// Takes over a freshly created plugin. `instrument` is whether it
    /// describes itself as one.
    ///
    /// Fails if the plugin cannot be brought up with the model's parameter
    /// values in place; it is then torn down again.
    pub(crate) fn new(
        main: MainThread<PluginThread>,
        name: String,
        instrument: bool,
        created: Created,
        (sample_rate, max_block): (f64, usize),
        (patient, keyed): (bool, bool),
    ) -> Result<Self, String> {
        let max_block = max_block.max(1);
        let key_bus = crate::signal::key_port(&created.buses.inputs, instrument);
        let mut processor = Self {
            patient,
            key_bus,
            key_on: key_bus == Some(0) || (keyed && key_bus.is_some()),
            offline: created.offline,
            audio: Some(created.audio),
            active: true,
            processing: false,
            failed: false,
            main,
            instance: created.id,
            name,
            sample_rate,
            max_block,
            latency: created.latency,
            additive: instrument || created.buses.inputs.is_empty(),
            inputs: BusBuffers::inputs(&created.buses.inputs, max_block),
            outputs: BusBuffers::outputs(&created.buses.outputs, max_block),
            layout: created.buses,
            events: BlockEvents::new(created.takes_notes, created.midi),
            output_events: EventList::with_capacity(0),
            params: BlockParams::new(&created.params, created.links, created.silencing),
            continuous_samples: 0,
        };
        processor.start_from(&created.initial_values)?;
        Ok(processor)
    }

    /// Puts the model's parameter values in place before the first block.
    ///
    /// A VST3 processor only takes parameter values through `process`. So
    /// they are sent in a block of no frames, and the plugin is then
    /// activated again: a plugin takes the values it has when activated as
    /// its starting point, while to values that arrive later it glides,
    /// audibly, at the start of a render.
    fn start_from(&mut self, values: &[(usize, f64)]) -> Result<(), String> {
        if values.is_empty() {
            return Ok(());
        }
        let stopped = ProcessInfo {
            sample_rate: self.sample_rate,
            tempo: 120.0,
            time_signature: Default::default(),
            playing: false,
            position_beats: 0.0,
            song: Default::default(),
            loop_range: None,
        };
        let set_all =
            |processor: &mut Self| values.iter().for_each(|&(index, value)| processor.params.set(index, value));
        set_all(self);
        // Whether or not the plugin makes anything of a block without
        // frames, the values go out again with the first real one.
        self.call(&stopped, &[], (0, 0, 0), false);
        self.reactivate(self.sample_rate, self.max_block)?;
        set_all(self);
        Ok(())
    }

    /// Tells the plugin that processing has stopped, as VST3 requires
    /// before it is deactivated.
    fn stop(&mut self) {
        if let (true, Some(audio)) = (std::mem::take(&mut self.processing), &self.audio) {
            // SAFETY: the plugin is live and was told processing started.
            // `setProcessing` belongs to the audio thread; nothing else is
            // processing this plugin (`&mut self`), so this thread is its
            // audio thread for the duration.
            unsafe { audio.setProcessing(0) };
        }
    }

    /// Deactivates the plugin and activates it again, on its main thread,
    /// for the given configuration. On failure the plugin is left inactive
    /// and the device passes audio through.
    fn reactivate(&mut self, sample_rate: f64, max_block: usize) -> Result<(), String> {
        self.stop();
        let instance = self.instance;
        let block = u32::try_from(max_block).unwrap_or(u32::MAX);
        let asked = std::time::Instant::now();
        let result = self.main.call(REACTIVATE_TIMEOUT, move |thread| {
            thread.vst3.reactivate(instance, (sample_rate, block), asked.elapsed())
        });
        let result = match result {
            Ok(Ok(latency)) => {
                self.latency = latency;
                self.failed = false;
                Ok(())
            }
            Ok(Err(reason)) => Err(reason),
            Err(error) => Err(format!("cannot re-activate: {error}")),
        };
        self.active = result.is_ok();
        self.sample_rate = sample_rate;
        self.max_block = max_block;
        self.events.forget_notes();
        result
    }

    /// Calls the plugin's `process` for the frames `start..start + frames`
    /// of a block of `total` frames, given as `part`. With `with_audio`
    /// the plugin's buses are passed, loaded by the caller; without, the
    /// call carries events and parameters only and `frames` is 0, which is
    /// how VST3 delivers parameters to a plugin that is not being played.
    /// Returns whether the plugin succeeded.
    fn call(&mut self, info: &ProcessInfo, events: &[Event], part: (usize, usize, usize), with_audio: bool) -> bool {
        let Some(audio) = &self.audio else { return false };
        let (start, frames, _) = part;
        if !std::mem::replace(&mut self.processing, true) {
            // The first block starts the plugin. Plugins that have nothing
            // to do here say "not implemented", so the answer is ignored.
            // SAFETY: the plugin is live and active, and this thread is
            // its audio thread.
            unsafe { audio.setProcessing(1) };
        }

        let silencing = self.events.fill(info, events, part);
        self.params.fill(silencing.then(|| self.events.spoken()), self.events.controls());
        let (input_changes, output_changes) = self.params.lists();
        let mut context = process_context(info, start, self.continuous_samples);
        let mut data = ProcessData {
            processMode: super::instance::process_mode(self.offline),
            symbolicSampleSize: kSample32 as int32,
            numSamples: frames as int32,
            numInputs: if with_audio { self.inputs.count() } else { 0 },
            numOutputs: if with_audio { self.outputs.count() } else { 0 },
            inputs: if with_audio { self.inputs.prepare() } else { null_mut() },
            outputs: if with_audio { self.outputs.prepare() } else { null_mut() },
            inputParameterChanges: input_changes,
            outputParameterChanges: output_changes,
            inputEvents: self.events.list(),
            outputEvents: interface(&self.output_events),
            processContext: &mut context,
        };
        // SAFETY: the plugin is live, active and processing, and this
        // thread is its audio thread. Every pointer in `data` is null or
        // points to something this processor owns or to a local, valid
        // for the whole call: the bus structs and their buffers hold at
        // least `frames` frames, and the lists are objects of this host.
        let succeeded = unsafe { audio.process(&mut data) } == kResultOk;
        self.continuous_samples += frames as i64;
        if succeeded {
            self.params.collect();
        }
        succeeded
    }

    /// Runs the plugin over one part of a block. `left` and `right` are
    /// that part and no longer than the plugin's maximum block size, and
    /// `key` that part of what the device listens to, if it listens.
    fn process_part(
        &mut self,
        info: &ProcessInfo,
        events: &[Event],
        part: (usize, usize, usize),
        left: &mut [f32],
        right: &mut [f32],
        key: Option<KeyInput<'_>>,
    ) {
        self.inputs.load(left, right);
        // Only into a bus that is switched on: what is in one that is not
        // is nothing a plugin has been told to expect.
        if let (Some(bus), true, Some(key)) = (self.key_bus, self.key_on, key) {
            self.inputs.load_key(bus, key.left, key.right);
        }
        // A plugin may return without writing its outputs; what it leaves
        // must not be the previous block.
        self.outputs.clear(left.len());
        if self.call(info, events, part, true) {
            self.outputs.unload(left, right, self.additive);
        } else {
            // The device passes its input through untouched from here on.
            // Logging is not for the audio thread; the main thread reports
            // the failure when it next idles.
            self.failed = true;
            self.params.report_failure();
        }
    }
}

impl Vst3Processor {
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
        if total == 0 || self.failed || !self.active {
            return;
        }
        // The engine never exceeds `max_block`, the size the plugin was
        // set up for. Should a caller do so anyway, feed the plugin in
        // pieces it can take.
        let mut start = 0;
        while start < total && !self.failed {
            let end = (start + self.max_block).min(total);
            let part = (start, end - start, total);
            let key = key.map(|key| KeyInput { left: &key.left[start..end], right: &key.right[start..end] });
            self.process_part(info, events, part, &mut left[start..end], &mut right[start..end], key);
            start = end;
        }
    }
}

impl DeviceProcessor for Vst3Processor {
    /// The factory activates the plugin, so this only has work to do when
    /// the configuration changes: a VST3 plugin must be deactivated, set
    /// up and activated again, on its main thread, to change sample rate.
    fn prepare(&mut self, sample_rate: f64, max_block: usize) {
        let max_block = max_block.max(1);
        if sample_rate == self.sample_rate && max_block == self.max_block {
            return;
        }
        if let Err(reason) = self.reactivate(sample_rate, max_block) {
            log::error!("[{}] {reason}; the device is bypassed", self.name);
        }
        self.inputs = BusBuffers::inputs(&self.layout.inputs, max_block);
        self.outputs = BusBuffers::outputs(&self.layout.outputs, max_block);
    }

    fn set_param(&mut self, index: usize, value: f64) {
        self.params.set(index, value);
    }

    fn process(&mut self, info: &ProcessInfo, events: &[Event], left: &mut [f32], right: &mut [f32]) {
        self.run(info, events, left, right, None);
    }

    fn takes_key(&self) -> bool {
        self.key_bus.is_some()
    }

    fn key_is_set_when_made(&self) -> bool {
        self.key_bus.is_some_and(|bus| bus > 0)
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

    /// VST3 has no call that silences a plugin. The nearest things are
    /// used together: processing is stopped and started again, which
    /// plugins take as the cue to clear their buffers, and with the next
    /// block the keys this host pressed are released and the plugin is
    /// sent "all sound off" if it takes MIDI controllers. Tails inside a
    /// plugin that ignores all of that cannot be cleared.
    fn reset(&mut self) {
        self.events.silence();
        if let (true, false, Some(audio)) = (self.processing, self.failed, &self.audio) {
            // SAFETY: the plugin is live, active and processing, and this
            // thread is its audio thread; `setProcessing` is the one state
            // change VST3 allows from here.
            unsafe {
                audio.setProcessing(0);
                audio.setProcessing(1);
            }
        }
    }

    fn latency_samples(&self) -> u32 {
        self.latency
    }
}

impl Drop for Vst3Processor {
    /// Tears the plugin down in the order VST3 requires: stop processing
    /// here, then deactivate, terminate and release on the plugin main
    /// thread. The processing interface goes with the job, so that the
    /// plugin's last reference is released there and not here.
    ///
    /// This waits for the teardown, so that once a device is dropped its
    /// plugin is really gone, and a DAW shutting down does not exit in the
    /// middle of it. (A plugin that has to be left alone for a while
    /// first is gone a little later, on the plugin main thread; see
    /// `Vst3Thread::destroy`.) The plugin main thread outlives the factory
    /// for as long as any processor holds a handle to it, so this cannot
    /// be left waiting on a thread that has already stopped.
    fn drop(&mut self) {
        self.stop();
        let (instance, audio, patient) = (self.instance, self.audio.take(), self.patient);
        let destroy = move |thread: &mut PluginThread| thread.vst3.destroy(instance, audio, patient);
        // At rest: a plugin is not taken away from under an event loop
        // that its own code may have started.
        if let Err(error) = self.main.call_at_rest(TEARDOWN_TIMEOUT, destroy) {
            log::warn!("[{}] was not torn down cleanly: {error}", self.name);
        }
    }
}
