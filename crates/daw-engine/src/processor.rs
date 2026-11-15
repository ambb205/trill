//! The audio-thread core: turns a [`Graph`] into samples.
//!
//! Nothing in here allocates, locks or blocks once constructed. The same
//! processor drives realtime playback and offline rendering, so a bounce is
//! exactly what you heard.

use crate::graph::{Chain, Graph, MAX_BLOCK, TrackNode, TrackState};
use daw_core::device::{Event, EventKind, MAX_EVENTS_PER_BLOCK, ProcessInfo};
use daw_core::project::{MIN_VOLUME_DB, automation_value};
use daw_core::{DeviceId, TrackId};
use daw_dsp::util::{db_to_gain, pan_gains};

const EVENT_CAPACITY: usize = MAX_EVENTS_PER_BLOCK;
/// Leave room for the note-offs that follow a burst of note-ons.
const NOTE_ON_LIMIT: usize = EVENT_CAPACITY - 512;
const LIVE_CAPACITY: usize = 128;
/// Fade applied at the edges of audio clips to avoid clicks.
const CLIP_FADE_SECONDS: f64 = 0.003;

pub struct Processor {
    graph: Box<Graph>,
    sample_rate: f64,
    playing: bool,
    /// Song position is `anchor_beat` plus `frames` at the current tempo.
    /// Anchoring keeps positions exact however long playback runs.
    anchor_beat: f64,
    frames: u64,
    /// Release every sounding note at the start of the next block.
    flush: bool,
    /// Notes played live (auditioning), delivered at the next block.
    live: Vec<(TrackId, EventKind)>,
    events: Vec<Event>,
    track_left: Vec<f32>,
    track_right: Vec<f32>,
    mix_left: Vec<f32>,
    mix_right: Vec<f32>,
}

impl Processor {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            graph: Box::new(Graph::empty()),
            sample_rate,
            playing: false,
            anchor_beat: 0.0,
            frames: 0,
            flush: false,
            live: Vec::with_capacity(LIVE_CAPACITY),
            events: Vec::with_capacity(EVENT_CAPACITY),
            track_left: vec![0.0; MAX_BLOCK],
            track_right: vec![0.0; MAX_BLOCK],
            mix_left: vec![0.0; MAX_BLOCK],
            mix_right: vec![0.0; MAX_BLOCK],
        }
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    fn beats_per_frame(&self) -> f64 {
        self.graph.tempo / (60.0 * self.sample_rate)
    }

    /// Current song position in beats.
    pub fn position(&self) -> f64 {
        self.anchor_beat + self.frames as f64 * self.beats_per_frame()
    }

    pub fn play(&mut self) {
        self.playing = true;
    }

    /// Stops the transport. Sounding notes are released and tails ring out.
    pub fn stop(&mut self) {
        if self.playing {
            self.playing = false;
            self.flush = true;
        }
    }

    pub fn seek(&mut self, beat: f64) {
        self.anchor_beat = beat.max(0.0);
        self.frames = 0;
        self.flush = true;
    }

    /// Silences everything immediately, including reverb and delay tails.
    pub fn panic(&mut self) {
        self.flush = true;
        for track in &mut self.graph.tracks {
            for slot in &mut track.chain.devices {
                if let Some(processor) = slot.processor.as_mut() {
                    processor.reset();
                }
            }
        }
        for slot in &mut self.graph.master.chain.devices {
            if let Some(processor) = slot.processor.as_mut() {
                processor.reset();
            }
        }
    }

    pub fn set_param(&mut self, device: DeviceId, index: usize, value: f64) {
        if let Some(processor) = self.graph.processor_mut(device) {
            processor.set_param(index, value);
        }
    }

    /// Queues a live note for `track`. Dropped if the queue is full.
    pub fn live_event(&mut self, track: TrackId, kind: EventKind) {
        if self.live.len() < self.live.capacity() {
            self.live.push((track, kind));
        }
    }

    /// Installs `new`, carrying running state across, and returns the old
    /// graph so the caller can free it off the audio thread.
    pub fn swap_graph(&mut self, mut new: Box<Graph>) -> Box<Graph> {
        let position = self.position();
        new.adopt(&mut self.graph);
        let tempo_changed = new.tempo != self.graph.tempo;
        let old = std::mem::replace(&mut self.graph, new);
        if tempo_changed {
            self.anchor_beat = position;
            self.frames = 0;
        }
        old
    }

    /// Renders `left.len()` frames. The slices must have equal length.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let _denormals = DenormalGuard::new();
        let total = left.len().min(right.len());
        let mut done = 0;
        while done < total {
            let mut frames = (total - done).min(MAX_BLOCK);

            // End the block exactly at the loop end so the wrap is sample
            // accurate.
            let mut wrap = None;
            if self.playing
                && let Some((start, end)) = self.graph.loop_region
            {
                let position = self.position();
                if position < end {
                    // The small bias absorbs float rounding, so a loop that
                    // is a whole number of frames long stays exactly that.
                    let until_end = ((end - position) / self.beats_per_frame() - 1e-6).ceil().max(1.0);
                    if until_end <= frames as f64 {
                        frames = until_end as usize;
                        wrap = Some((start, end));
                    }
                }
            }

            let limit = wrap.map(|(_, end)| end);
            self.process_block(&mut left[done..done + frames], &mut right[done..done + frames], limit);
            done += frames;

            if self.playing {
                self.frames += frames as u64;
                if let Some((start, _)) = wrap {
                    // Restart exactly on the loop start. The block above ran
                    // to the first frame at or past the loop end, so a loop
                    // is at most one frame longer than its length in beats,
                    // and a note sitting on the loop start is never missed.
                    self.anchor_beat = start;
                    self.frames = 0;
                    self.flush = true;
                }
            }
        }
    }

    /// Renders one block. `limit` caps the beat range that may trigger
    /// notes, so nothing past a loop end sounds before the wrap.
    fn process_block(&mut self, out_left: &mut [f32], out_right: &mut [f32], limit: Option<f64>) {
        let frames = out_left.len();
        let start_beat = self.position();
        let beats_per_frame = self.beats_per_frame();
        // Computed the same way as the next block's start, so consecutive
        // blocks tile the timeline exactly and a note on a block boundary is
        // played once: never dropped, never doubled.
        let next_start = self.anchor_beat + (self.frames + frames as u64) as f64 * beats_per_frame;
        let end_beat = limit.map_or(next_start, |limit| next_start.min(limit));
        let info = ProcessInfo {
            sample_rate: self.sample_rate,
            tempo: self.graph.tempo,
            time_signature: self.graph.time_signature,
            playing: self.playing,
            position_beats: start_beat,
            loop_range: self.graph.loop_region,
        };
        let flush = std::mem::take(&mut self.flush);
        let playing = self.playing;

        let mix_left = &mut self.mix_left[..frames];
        let mix_right = &mut self.mix_right[..frames];
        mix_left.fill(0.0);
        mix_right.fill(0.0);

        for track in &mut self.graph.tracks {
            let left = &mut self.track_left[..frames];
            let right = &mut self.track_right[..frames];
            left.fill(0.0);
            right.fill(0.0);

            let events = &mut self.events;
            events.clear();
            if flush {
                events.push(Event { offset: 0, kind: EventKind::AllNotesOff });
                track.state.active_until = [-1.0; 128];
                track.state.active_count = 0;
            }
            for (id, kind) in &self.live {
                if *id == track.id {
                    events.push(Event { offset: 0, kind: *kind });
                }
            }
            if playing {
                collect_notes(track, start_beat, end_beat, beats_per_frame, frames, events);
                render_audio(track, start_beat, beats_per_frame, self.sample_rate, left, right);
            }

            run_chain(&mut track.chain, &info, events, left, right);

            let volume = match &track.volume_automation {
                Some(points) => automation_value(points, start_beat) as f32,
                None => track.volume_db,
            };
            let pan = match &track.pan_automation {
                Some(points) => automation_value(points, start_beat) as f32,
                None => track.pan,
            };
            let (target_left, target_right) = if track.audible && volume > MIN_VOLUME_DB {
                let gain = db_to_gain(volume);
                let (pan_left, pan_right) = pan_gains(pan);
                (gain * pan_left, gain * pan_right)
            } else {
                (0.0, 0.0)
            };
            let state = &mut track.state;
            if !state.primed {
                state.gain_left = target_left;
                state.gain_right = target_right;
                state.primed = true;
            }
            let peak_left = mix_ramped(left, mix_left, state.gain_left, target_left);
            let peak_right = mix_ramped(right, mix_right, state.gain_right, target_right);
            state.gain_left = target_left;
            state.gain_right = target_right;
            track.meter.record(peak_left, peak_right);
        }
        self.live.clear();

        let master = &mut self.graph.master;
        run_chain(&mut master.chain, &info, &[], mix_left, mix_right);

        let volume = match &master.volume_automation {
            Some(points) => automation_value(points, start_beat) as f32,
            None => master.volume_db,
        };
        let target = if volume > MIN_VOLUME_DB { db_to_gain(volume) } else { 0.0 };
        if !master.primed {
            master.gain = target;
            master.primed = true;
        }
        out_left.fill(0.0);
        out_right.fill(0.0);
        let peak_left = mix_ramped(mix_left, out_left, master.gain, target);
        let peak_right = mix_ramped(mix_right, out_right, master.gain, target);
        master.gain = target;
        master.meter.record(peak_left, peak_right);
    }
}

/// Adds `source` into `destination` while ramping the gain from `from` to
/// `to`, and returns the peak of what was added.
#[inline]
fn mix_ramped(source: &[f32], destination: &mut [f32], from: f32, to: f32) -> f32 {
    let step = (to - from) / source.len().max(1) as f32;
    let mut gain = from;
    let mut peak = 0.0f32;
    for (out, sample) in destination.iter_mut().zip(source) {
        gain += step;
        let value = sample * gain;
        *out += value;
        peak = peak.max(value.abs());
    }
    peak
}

fn run_chain(chain: &mut Chain, info: &ProcessInfo, events: &[Event], left: &mut [f32], right: &mut [f32]) {
    let Chain { devices, automation } = chain;
    for lane in automation.iter_mut() {
        let value = automation_value(&lane.points, info.position_beats);
        if value != lane.last {
            lane.last = value;
            if let Some(processor) = devices.get_mut(lane.slot).and_then(|slot| slot.processor.as_mut()) {
                processor.set_param(lane.param, value);
            }
        }
    }
    for slot in devices.iter_mut() {
        if slot.enabled
            && let Some(processor) = slot.processor.as_mut()
        {
            processor.process(info, events, left, right);
        }
    }
}

/// Appends the note events of `track` that fall in `[start_beat, end_beat)`,
/// ordered by sample offset.
fn collect_notes(
    track: &mut TrackNode,
    start_beat: f64,
    end_beat: f64,
    beats_per_frame: f64,
    frames: usize,
    events: &mut Vec<Event>,
) {
    let first_new = events.len();
    let last = frames.saturating_sub(1) as f64;
    let offset_of = |beat: f64| ((beat - start_beat) / beats_per_frame).round().clamp(0.0, last) as u32;
    let state = &mut track.state;

    // Notes from earlier blocks that end in this one.
    let release_ended = |state: &mut TrackState, events: &mut Vec<Event>| {
        if state.active_count == 0 {
            return;
        }
        for pitch in 0..128usize {
            let end = state.active_until[pitch];
            if end >= 0.0 && end < end_beat {
                events.push(Event { offset: offset_of(end), kind: EventKind::NoteOff { pitch: pitch as u8 } });
                state.active_until[pitch] = -1.0;
                state.active_count -= 1;
            }
        }
    };
    release_ended(state, events);

    // Notes that start in this block. A pitch that is still sounding is
    // released first, so repeated notes retrigger cleanly.
    let first = track.notes.partition_point(|note| note.start < start_beat);
    let mut started = false;
    for note in &track.notes[first..] {
        if note.start >= end_beat || events.len() >= NOTE_ON_LIMIT {
            break;
        }
        let offset = offset_of(note.start);
        let pitch = note.pitch as usize;
        if state.active_until[pitch] >= 0.0 {
            events.push(Event { offset, kind: EventKind::NoteOff { pitch: note.pitch } });
        } else {
            state.active_count += 1;
        }
        state.active_until[pitch] = note.end;
        events.push(Event {
            offset,
            kind: EventKind::NoteOn { pitch: note.pitch, velocity: note.velocity as f32 / 127.0 },
        });
        started = true;
    }

    // Notes short enough to start and end inside this block.
    if started {
        release_ended(state, events);
    }

    // Stable insertion sort by offset. The three passes above were appended
    // in the order they must keep when offsets tie.
    let new = &mut events[first_new..];
    for i in 1..new.len() {
        let mut j = i;
        while j > 0 && new[j - 1].offset > new[j].offset {
            new.swap(j - 1, j);
            j -= 1;
        }
    }
}

/// Mixes the audio clips of `track` into the block.
fn render_audio(
    track: &TrackNode,
    start_beat: f64,
    beats_per_frame: f64,
    sample_rate: f64,
    left: &mut [f32],
    right: &mut [f32],
) {
    let frames = left.len();
    let end_beat = start_beat + frames as f64 * beats_per_frame;
    let fade = CLIP_FADE_SECONDS * sample_rate;

    for region in &track.audio {
        if region.end <= start_beat || region.start >= end_beat {
            continue;
        }
        let data = &region.data;
        let available = data.left.len().min(data.right.len());
        let ratio = data.sample_rate / sample_rate;
        // Output frames between the clip start and this block (negative if
        // the clip starts inside the block).
        let elapsed = (start_beat - region.start) / beats_per_frame;
        let length = (region.end - region.start) / beats_per_frame;
        // The clip's own fades, never shorter than the click guard.
        let fade_in = (region.fade_in / beats_per_frame).max(fade);
        let fade_out = (region.fade_out / beats_per_frame).max(fade);
        let first = (-elapsed).ceil().max(0.0) as usize;
        let last = ((length - elapsed).ceil().max(0.0) as usize).min(frames);

        for i in first..last {
            let t = elapsed + i as f64;
            let source = region.offset_frames + t * ratio;
            let index = source as usize;
            if index + 1 >= available {
                break;
            }
            let fraction = (source - index as f64) as f32;
            let envelope = (t / fade_in).min((length - t) / fade_out).clamp(0.0, 1.0) as f32;
            let gain = region.gain * envelope;
            left[i] += (data.left[index] + (data.left[index + 1] - data.left[index]) * fraction) * gain;
            right[i] += (data.right[index] + (data.right[index + 1] - data.right[index]) * fraction) * gain;
        }
    }
}

/// Flushes denormal floats to zero while alive. Decaying filters and reverb
/// tails otherwise drift into subnormals, which are orders of magnitude
/// slower to compute and show up as CPU spikes in silence.
struct DenormalGuard {
    #[cfg(target_arch = "x86_64")]
    saved: u32,
}

impl DenormalGuard {
    #[cfg(target_arch = "x86_64")]
    fn new() -> Self {
        let mut saved = 0u32;
        // SAFETY: stmxcsr/ldmxcsr only read and write the SSE control
        // register through a valid pointer; the previous value is restored on
        // drop, so no Rust code outside this scope observes the change.
        unsafe {
            std::arch::asm!("stmxcsr [{}]", in(reg) &mut saved, options(nostack, preserves_flags));
            // Flush-to-zero (bit 15) and denormals-are-zero (bit 6).
            let flushed = saved | 0x8040;
            std::arch::asm!("ldmxcsr [{}]", in(reg) &flushed, options(nostack, preserves_flags, readonly));
        }
        Self { saved }
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn new() -> Self {
        Self {}
    }
}

impl Drop for DenormalGuard {
    fn drop(&mut self) {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: restores the control register saved in `new`.
        unsafe {
            std::arch::asm!("ldmxcsr [{}]", in(reg) &self.saved, options(nostack, preserves_flags, readonly));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Meter, NoteEvent};
    use std::sync::Arc;

    fn track(notes: &[(f64, f64, u8)]) -> TrackNode {
        TrackNode {
            id: TrackId(1),
            notes: notes.iter().map(|&(start, end, pitch)| NoteEvent { start, end, pitch, velocity: 127 }).collect(),
            audio: Vec::new(),
            chain: Chain::default(),
            volume_db: 0.0,
            pan: 0.0,
            audible: true,
            volume_automation: None,
            pan_automation: None,
            meter: Arc::new(Meter::default()),
            state: TrackState::default(),
        }
    }

    /// Runs `blocks` blocks of 100 frames at 0.01 beats per frame and
    /// returns `(absolute frame, is note-on, pitch)` for every event.
    fn events_of(track: &mut TrackNode, blocks: usize) -> Vec<(usize, bool, u8)> {
        let mut out = Vec::new();
        let mut events = Vec::with_capacity(EVENT_CAPACITY);
        for block in 0..blocks {
            events.clear();
            let start = block as f64;
            collect_notes(track, start, start + 1.0, 0.01, 100, &mut events);
            for event in &events {
                let frame = block * 100 + event.offset as usize;
                match event.kind {
                    EventKind::NoteOn { pitch, .. } => out.push((frame, true, pitch)),
                    EventKind::NoteOff { pitch } => out.push((frame, false, pitch)),
                    EventKind::AllNotesOff => {}
                }
            }
        }
        out
    }

    #[test]
    fn back_to_back_notes_release_before_they_retrigger() {
        // Two legato notes on one pitch, meeting mid-block and on a boundary.
        let mut track = track(&[(0.25, 0.5, 60), (0.5, 1.0, 60), (1.0, 1.5, 60)]);
        assert_eq!(
            events_of(&mut track, 3),
            [(25, true, 60), (50, false, 60), (50, true, 60), (100, false, 60), (100, true, 60), (150, false, 60)]
        );
        assert_eq!(track.state.active_count, 0);
    }

    #[test]
    fn notes_shorter_than_a_block_start_and_stop_inside_it() {
        let mut track = track(&[(0.10, 0.15, 60), (0.12, 0.90, 64), (0.50, 0.5001, 67)]);
        assert_eq!(
            events_of(&mut track, 1),
            [(10, true, 60), (12, true, 64), (15, false, 60), (50, true, 67), (50, false, 67), (90, false, 64)]
        );
    }

    #[test]
    fn overlapping_notes_on_one_pitch_leave_nothing_hanging() {
        let mut track = track(&[(0.0, 2.5, 60), (1.2, 1.4, 60)]);
        assert_eq!(events_of(&mut track, 3), [(0, true, 60), (120, false, 60), (120, true, 60), (140, false, 60)]);
        assert_eq!(track.state.active_count, 0);
    }
}
