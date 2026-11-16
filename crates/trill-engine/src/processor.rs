//! The audio-thread core: turns a [`Graph`] into samples.
//!
//! Nothing in here allocates, locks or blocks once constructed. The same
//! processor drives realtime playback and offline rendering, so a bounce is
//! exactly what you heard.

use crate::engine::RecordPhase;
use crate::graph::{AudioData, Chain, Graph, MAX_BLOCK, RegionAudio, TrackNode, TrackState};
use crate::live::{Captured, LiveTarget, TrackSet};
use crate::monitor::{Beats, Monitor};
use trill_core::device::{Event, EventKind, MAX_EVENTS_PER_BLOCK, ProcessInfo};
use trill_core::project::{MIN_VOLUME_DB, automation_value};
use trill_core::{DeviceId, TrackId};
use trill_dsp::util::{db_to_gain, pan_gains};

const EVENT_CAPACITY: usize = MAX_EVENTS_PER_BLOCK;
/// Leave room for the note-offs that follow a burst of note-ons.
const NOTE_ON_LIMIT: usize = EVENT_CAPACITY - 512;
/// Live events held for one callback. A player with ten fingers, two feet
/// and a pitch wheel does not come near it.
const LIVE_CAPACITY: usize = 1024;
/// Fade applied at the edges of audio clips to avoid clicks.
const CLIP_FADE_SECONDS: f64 = 0.003;
/// Length of the cross-fade from a stand-in to the audio that replaces it.
const HAND_OVER_SECONDS: f64 = 0.01;

/// Which sounding notes to release at the start of the next block.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Flush {
    Nothing,
    /// The notes of the song, because it jumped (a seek, the loop coming
    /// round). What is being played by hand is left alone: a chord held
    /// across the loop point keeps sounding.
    Song,
    /// Everything, held keys included: the transport stopped.
    Everything,
}

/// Something played by hand, waiting for its block.
#[derive(Copy, Clone)]
struct LiveEvent {
    target: LiveTarget,
    kind: EventKind,
    /// Frames from the start of the next `process` call.
    offset: u32,
    /// How many frames before the frame it is placed at it was played.
    early: f64,
}

/// A take in progress.
#[derive(Copy, Clone)]
struct Recording {
    tracks: TrackSet,
    /// Frames of count-in still to go; the song starts when it reaches 0.
    count_in: u64,
    /// Added to the count-in's positions for the metronome, a whole number
    /// of bars, so that counting in to the start of the song does not ask
    /// it for clicks at negative positions.
    click_shift: f64,
    /// How many times the loop has come round during the take.
    pass: u32,
}

pub struct Processor {
    graph: Box<Graph>,
    sample_rate: f64,
    playing: bool,
    /// Song position is `anchor_beat` plus `frames` at the current tempo.
    /// Anchoring keeps positions exact however long playback runs.
    anchor_beat: f64,
    frames: u64,
    /// What to release at the start of the next block.
    flush: Flush,
    /// What was played by hand since the last callback.
    live: Vec<LiveEvent>,
    /// The tracks a keyboard plays.
    live_targets: TrackSet,
    events: Vec<Event>,
    track_left: Vec<f32>,
    track_right: Vec<f32>,
    mix_left: Vec<f32>,
    mix_right: Vec<f32>,
    /// The audition voice and the metronome, heard but never rendered.
    monitor: Monitor,
    /// Frames between rendering a frame and hearing it.
    output_latency: f64,
    recording: Option<Recording>,
    /// Where what is recorded goes; `None` when rendering offline.
    capture: Option<rtrb::Producer<Captured>>,
    /// Frames played since the song position last jumped (or playback
    /// began), and where the song was just before: for telling where the
    /// song was heard to be a moment ago.
    continuity: u64,
    before_jump: f64,
    jump_was_wrap: bool,
}

impl Processor {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            graph: Box::new(Graph::empty()),
            sample_rate,
            playing: false,
            anchor_beat: 0.0,
            frames: 0,
            flush: Flush::Nothing,
            live: Vec::with_capacity(LIVE_CAPACITY),
            live_targets: TrackSet::default(),
            events: Vec::with_capacity(EVENT_CAPACITY),
            track_left: vec![0.0; MAX_BLOCK],
            track_right: vec![0.0; MAX_BLOCK],
            mix_left: vec![0.0; MAX_BLOCK],
            mix_right: vec![0.0; MAX_BLOCK],
            monitor: Monitor::new(sample_rate),
            output_latency: 0.0,
            recording: None,
            capture: None,
            continuity: 0,
            before_jump: 0.0,
            jump_was_wrap: false,
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
        // Asked to play in the middle of a count-in: the count-in is over.
        if let Some(recording) = &mut self.recording
            && recording.count_in > 0
        {
            recording.count_in = 0;
            self.report(Captured::Rolling { beat: self.position() });
        }
        if !self.playing {
            self.playing = true;
            self.jumped(self.position(), false);
        }
    }

    /// Stops the transport. Sounding notes are released, those held by
    /// hand included (stopping a song that is not playing is how to
    /// silence a key that hangs), and tails ring out. A take that was being
    /// recorded ends here.
    pub fn stop(&mut self) {
        if let Some(heard) = self.heard() {
            // Where the song was heard to be when it was stopped.
            let (beat, pass) = heard.at(0, 0.0);
            self.recording = None;
            self.report(Captured::Stopped { beat, pass });
        }
        self.playing = false;
        self.flush = Flush::Everything;
    }

    /// Lets go of every note that sounds, played by hand or by the song,
    /// at the start of the next block.
    pub(crate) fn release_everything(&mut self) {
        self.flush = Flush::Everything;
    }

    /// What it takes to say where the song is heard to be right now, while
    /// a take is being recorded.
    fn heard(&self) -> Option<Heard> {
        self.recording.map(|recording| Heard {
            tracks: recording.tracks,
            pass: recording.pass,
            start_beat: self.position(),
            beats_per_frame: self.beats_per_frame(),
            count_in: recording.count_in as f64,
            latency: self.output_latency,
            continuity: self.continuity as f64,
            before_jump: self.before_jump,
            jump_was_wrap: self.jump_was_wrap,
        })
    }

    pub fn seek(&mut self, beat: f64) {
        let before = self.position();
        self.anchor_beat = beat.max(0.0);
        self.frames = 0;
        self.flush = self.flush.max(Flush::Song);
        self.jumped(before, false);
    }

    /// Notes that the song position is no longer where the frames played
    /// so far lead to: it was at `before` and is now somewhere else.
    fn jumped(&mut self, before: f64, wrap: bool) {
        self.continuity = 0;
        self.before_jump = before;
        self.jump_was_wrap = wrap;
    }

    /// Silences everything immediately, including reverb and delay tails.
    pub fn panic(&mut self) {
        self.flush = Flush::Everything;
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

    /// Starts auditioning a sound over the song. Returns the buffer that was
    /// being auditioned before, to be freed off the audio thread.
    pub fn start_audition(&mut self, audio: std::sync::Arc<AudioData>, gain: f32) -> Option<std::sync::Arc<AudioData>> {
        self.monitor.start_audition(audio, gain)
    }

    pub fn stop_audition(&mut self) {
        self.monitor.stop_audition();
    }

    /// The buffer of an audition that has ended, to be freed off the audio
    /// thread.
    pub fn take_finished_audition(&mut self) -> Option<std::sync::Arc<AudioData>> {
        self.monitor.take_finished()
    }

    pub fn is_auditioning(&self) -> bool {
        self.monitor.is_auditioning()
    }

    pub fn set_metronome(&mut self, on: bool) {
        self.monitor.set_metronome(on);
    }

    /// How long after a frame is rendered it is heard, in frames. Takes are
    /// written where the player heard the song to be, which is this much
    /// behind where the engine is.
    pub fn set_output_latency(&mut self, frames: u32) {
        self.output_latency = frames as f64;
    }

    /// Where a take's events are reported. Returns where they went before.
    pub(crate) fn set_capture(
        &mut self,
        capture: Option<rtrb::Producer<Captured>>,
    ) -> Option<rtrb::Producer<Captured>> {
        std::mem::replace(&mut self.capture, capture)
    }

    fn report(&mut self, captured: Captured) {
        if let Some(capture) = &mut self.capture {
            // A full queue loses the report: nobody is collecting.
            let _ = capture.push(captured);
        }
    }

    /// Plays `kind` on `track` at the start of the next block. Dropped if
    /// more is waiting than the processor holds.
    pub fn live_event(&mut self, track: TrackId, kind: EventKind) {
        self.live_on(track, kind, 0, 0.0);
    }

    /// Plays `kind` on `track`, `offset` frames into the next call to
    /// [`Processor::process`]. It was played `early` frames before that
    /// frame. Returns `false` if it was dropped for want of room.
    pub(crate) fn live_on(&mut self, track: TrackId, kind: EventKind, offset: u32, early: f64) -> bool {
        self.push_live(LiveEvent { target: LiveTarget::Track(track), kind, offset, early })
    }

    /// Like [`Processor::live_on`], on the tracks a keyboard plays.
    pub(crate) fn live_routed(&mut self, kind: EventKind, offset: u32, early: f64) -> bool {
        self.push_live(LiveEvent { target: LiveTarget::Routed, kind, offset, early })
    }

    fn push_live(&mut self, event: LiveEvent) -> bool {
        let room = self.live.len() < self.live.capacity();
        if room {
            self.live.push(event);
        }
        room
    }

    /// Sets the tracks a keyboard plays. A track that no longer is one lets
    /// go of what the keyboard was holding on it.
    pub(crate) fn set_live_targets(&mut self, targets: TrackSet) {
        for track in &mut self.graph.tracks {
            if self.live_targets.contains(track.id) && !targets.contains(track.id) {
                track.state.release_routed |= track.state.holds_routed();
            }
        }
        self.live_targets = targets;
    }

    /// Lets go of everything a keyboard is holding, on every track.
    pub(crate) fn release_live(&mut self) {
        for track in &mut self.graph.tracks {
            track.state.release_routed |= track.state.holds_routed();
        }
    }

    /// Starts the take numbered `take` on `tracks`. If the song is stopped
    /// it starts playing, after `count_in_beats` of metronome; if it is
    /// playing, the take begins where the song is.
    pub(crate) fn record(&mut self, take: u32, tracks: TrackSet, count_in_beats: f64) {
        self.report(Captured::Began { take });
        let signature = self.graph.time_signature;
        let bar = signature.numerator as f64 * 4.0 / signature.denominator as f64;
        let count_in_beats = if self.playing || !count_in_beats.is_finite() { 0.0 } else { count_in_beats.max(0.0) };
        let count_in = (count_in_beats / self.beats_per_frame()).round() as u64;
        let click_shift = if bar > 0.0 { (count_in_beats / bar).ceil() * bar } else { 0.0 };
        self.recording = Some(Recording { tracks, count_in, click_shift, pass: 0 });
        if count_in == 0 {
            self.play();
            self.report(Captured::Rolling { beat: self.position() });
        }
    }

    pub(crate) fn record_phase(&self) -> RecordPhase {
        match &self.recording {
            None => RecordPhase::Off,
            Some(recording) if recording.count_in > 0 => RecordPhase::CountIn,
            Some(_) => RecordPhase::Rolling,
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

            // End the block exactly where a count-in ends, so the song
            // starts on the sample.
            let counting = self.recording.map_or(0, |recording| recording.count_in);
            let starts = counting > 0 && counting <= frames as u64;
            if starts {
                frames = counting as usize;
            }

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
            self.process_block(&mut left[done..done + frames], &mut right[done..done + frames], limit, done);
            done += frames;

            if self.playing {
                self.frames += frames as u64;
                self.continuity += frames as u64;
                if let Some((start, _)) = wrap {
                    // Restart exactly on the loop start. The block above ran
                    // to the first frame at or past the loop end, so a loop
                    // is at most one frame longer than its length in beats,
                    // and a note sitting on the loop start is never missed.
                    let reached = self.position();
                    self.anchor_beat = start;
                    self.frames = 0;
                    self.flush = self.flush.max(Flush::Song);
                    self.jumped(reached, true);
                    let pass = self.recording.as_mut().map(|recording| {
                        recording.pass += 1;
                        recording.pass
                    });
                    if let Some(pass) = pass {
                        self.report(Captured::Wrapped { pass });
                    }
                }
            }
            if let Some(recording) = &mut self.recording {
                recording.count_in -= (frames as u64).min(recording.count_in);
            }
            if starts {
                self.play();
                self.report(Captured::Rolling { beat: self.position() });
            }
        }

        // What was placed in this call has been played; anything placed
        // beyond it waits for the next.
        let played = total as u32;
        self.live.retain_mut(|event| {
            let waiting = event.offset >= played;
            if waiting {
                event.offset -= played;
            }
            waiting
        });
    }

    /// Renders one block, which begins `block_start` frames into the
    /// current call to `process`. `limit` caps the beat range that may
    /// trigger notes, so nothing past a loop end sounds before the wrap.
    fn process_block(&mut self, out_left: &mut [f32], out_right: &mut [f32], limit: Option<f64>, block_start: usize) {
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
        let flush = std::mem::replace(&mut self.flush, Flush::Nothing);
        let playing = self.playing;
        let block = block_start as u32..(block_start + frames) as u32;
        let has_live = self.live.iter().any(|event| block.contains(&event.offset));
        // Where a take's events are written: see `Heard`.
        let heard = self.heard();

        let mix_left = &mut self.mix_left[..frames];
        let mix_right = &mut self.mix_right[..frames];
        mix_left.fill(0.0);
        mix_right.fill(0.0);

        let Graph { tracks, buses, .. } = &mut *self.graph;
        for bus in buses.iter_mut() {
            bus.left[..frames].fill(0.0);
            bus.right[..frames].fill(0.0);
        }

        for track in tracks.iter_mut() {
            let left = &mut self.track_left[..frames];
            let right = &mut self.track_right[..frames];
            // A group or return starts from what was fed to its bus; every
            // track that feeds it has already run.
            match track.input_bus.and_then(|bus| buses.get(bus)) {
                Some(bus) => {
                    left.copy_from_slice(&bus.left[..frames]);
                    right.copy_from_slice(&bus.right[..frames]);
                }
                None => {
                    left.fill(0.0);
                    right.fill(0.0);
                }
            }

            let events = &mut self.events;
            events.clear();
            release_notes(track, flush, events);
            let recorded = heard.as_ref().filter(|heard| heard.tracks.contains(track.id));
            let mut by_hand = false;
            if std::mem::take(&mut track.state.release_routed) {
                by_hand = true;
                let id = track.id;
                release_routed(track, events, |kind| {
                    if let (Some(heard), Some(capture)) = (recorded, &mut self.capture) {
                        let (beat, pass) = heard.at(0, 0.0);
                        let _ = capture.push(Captured::Event { track: id, kind, beat, pass });
                    }
                });
            }
            if has_live {
                for event in self.live.iter().filter(|event| block.contains(&event.offset)) {
                    let routed = event.target == LiveTarget::Routed;
                    let plays = match event.target {
                        LiveTarget::Routed => self.live_targets.contains(track.id),
                        LiveTarget::Track(id) => id == track.id,
                    };
                    if !plays || events.len() >= EVENT_CAPACITY {
                        continue;
                    }
                    // A key the keyboard does not hold here has nothing to
                    // let go of: it went down before this track was one it
                    // plays, or the note was ended for it already.
                    if let (true, EventKind::NoteOff { pitch }) = (routed, event.kind)
                        && track.state.routed_held & (1 << pitch.min(127)) == 0
                    {
                        continue;
                    }
                    by_hand = true;
                    let offset = event.offset - block.start;
                    track.state.played(event.kind, routed);
                    events.push(Event { offset, kind: event.kind });
                    let keeps = matches!(event.kind, EventKind::NoteOn { .. } | EventKind::NoteOff { .. })
                        || event.kind.sustain().is_some();
                    if let (true, Some(heard), Some(capture)) = (keeps, recorded, &mut self.capture) {
                        let (beat, pass) = heard.at(offset, event.early);
                        let _ = capture.push(Captured::Event { track: track.id, kind: event.kind, beat, pass });
                    }
                }
            }
            if playing {
                collect_notes(track, start_beat, end_beat, beats_per_frame, frames, events);
                let block = Block {
                    start_beat,
                    beats_per_frame,
                    anchor_frames: self.anchor_beat / beats_per_frame,
                    elapsed: self.frames,
                    sample_rate: self.sample_rate,
                };
                render_audio(track, &block, left, right);
            }
            if by_hand {
                // What is played by hand comes at any offset, and the
                // song's notes were added after it: devices want one list
                // in order. Ties keep the order they were added in.
                sort_by_offset(events);
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
            // Fade in place, so the output and every send carry the same
            // post-fader signal.
            let peak_left = fade_ramped(left, state.gain_left, target_left);
            let peak_right = fade_ramped(right, state.gain_right, target_right);
            state.gain_left = target_left;
            state.gain_right = target_right;
            track.meter.record(peak_left, peak_right);

            match track.output_bus.and_then(|bus| buses.get_mut(bus)) {
                Some(bus) => {
                    add_scaled(left, &mut bus.left[..frames], 1.0);
                    add_scaled(right, &mut bus.right[..frames], 1.0);
                }
                None => {
                    add_scaled(left, mix_left, 1.0);
                    add_scaled(right, mix_right, 1.0);
                }
            }
            for &(bus, gain) in &track.sends {
                if let Some(bus) = buses.get_mut(bus) {
                    add_scaled(left, &mut bus.left[..frames], gain);
                    add_scaled(right, &mut bus.right[..frames], gain);
                }
            }
        }

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

        let signature = self.graph.time_signature;
        let beats_from = |start: f64| Beats {
            start,
            per_frame: beats_per_frame,
            count: 4.0 / signature.denominator as f64,
            counts_per_bar: signature.numerator as u32,
        };
        match self.recording.filter(|recording| recording.count_in > 0) {
            // A count-in clicks whether or not the metronome is on, on the
            // song's own grid running up to where the take starts.
            Some(recording) => {
                let before = recording.count_in as f64 * beats_per_frame;
                self.monitor.mix(
                    out_left,
                    out_right,
                    Some(beats_from(start_beat - before + recording.click_shift)),
                    true,
                );
            }
            None => self.monitor.mix(out_left, out_right, playing.then(|| beats_from(start_beat)), false),
        }
    }
}

/// What it takes to say where the song was *heard* to be when something
/// was played: the take is written there, not where the engine had got to.
///
/// The engine runs ahead of the speakers by the output latency, and an
/// event reaches its block a moment after it was played. Both are known to
/// the frame, so both are taken off again. If that reaches back past a
/// jump of the song position (the loop coming round, a seek, the start of
/// playback), the answer is on the far side of the jump: a note played as
/// the loop was heard to end belongs at the loop's end, in the pass before.
struct Heard {
    tracks: TrackSet,
    pass: u32,
    start_beat: f64,
    beats_per_frame: f64,
    /// Frames of count-in left at the start of the block.
    count_in: f64,
    latency: f64,
    /// Frames played since the last jump, at the start of the block.
    continuity: f64,
    before_jump: f64,
    jump_was_wrap: bool,
}

impl Heard {
    /// The heard position, and the loop pass it belongs to, of an event
    /// placed `offset` frames into the block and played `early` frames
    /// before that.
    fn at(&self, offset: u32, early: f64) -> (f64, u32) {
        let offset = offset as f64;
        let back = early + self.latency;
        if self.count_in > 0.0 {
            // The song stands still at the take's start, which is this many
            // frames away.
            let until_start = (self.count_in - offset).max(0.0);
            return (self.start_beat - (until_start + back) * self.beats_per_frame, self.pass);
        }
        let since_jump = self.continuity + offset;
        if back <= since_jump {
            (self.start_beat + (offset - back) * self.beats_per_frame, self.pass)
        } else {
            let pass = if self.jump_was_wrap { self.pass.saturating_sub(1) } else { self.pass };
            (self.before_jump - (back - since_jump) * self.beats_per_frame, pass)
        }
    }
}

/// Releases what `flush` asks of a track at the start of a block.
fn release_notes(track: &mut TrackNode, flush: Flush, events: &mut Vec<Event>) {
    let state = &mut track.state;
    match flush {
        Flush::Nothing => {}
        // The song's notes go; the keys held by hand stay down.
        Flush::Song if state.live_held != 0 => {
            if state.active_count > 0 {
                for pitch in 0..128usize {
                    if state.active_until[pitch] >= 0.0 && state.live_held & (1 << pitch) == 0 {
                        events.push(Event { offset: 0, kind: EventKind::NoteOff { pitch: pitch as u8 } });
                    }
                }
            }
            state.active_until = [-1.0; 128];
            state.active_count = 0;
        }
        Flush::Song | Flush::Everything => {
            events.push(Event { offset: 0, kind: EventKind::AllNotesOff });
            state.active_until = [-1.0; 128];
            state.active_count = 0;
            state.forget_live();
        }
    }
}

/// Lets go of what a keyboard was holding on a track: every key, the
/// sustain pedal and the pitch wheel. `released` is told each event.
fn release_routed(track: &mut TrackNode, events: &mut Vec<Event>, mut released: impl FnMut(EventKind)) {
    let state = &mut track.state;
    let mut push = |kind: EventKind| {
        events.push(Event { offset: 0, kind });
        released(kind);
    };
    let held = std::mem::take(&mut state.routed_held);
    state.live_held &= !held;
    for pitch in (0..128u8).filter(|pitch| held & (1 << pitch) != 0) {
        push(EventKind::NoteOff { pitch });
    }
    if std::mem::take(&mut state.live_sustain) {
        push(EventKind::Control { controller: trill_core::device::CONTROLLER_SUSTAIN, value: 0.0 });
    }
    if std::mem::take(&mut state.live_bent) {
        push(EventKind::PitchBend { value: 0.0 });
    }
}

/// Stable insertion sort by offset: the lists are short and nearly sorted.
fn sort_by_offset(events: &mut [Event]) {
    for i in 1..events.len() {
        let mut j = i;
        while j > 0 && events[j - 1].offset > events[j].offset {
            events.swap(j - 1, j);
            j -= 1;
        }
    }
}

/// Applies a gain ramping from `from` to `to` in place and returns the
/// resulting peak.
#[inline]
fn fade_ramped(samples: &mut [f32], from: f32, to: f32) -> f32 {
    let step = (to - from) / samples.len().max(1) as f32;
    let mut gain = from;
    let mut peak = 0.0f32;
    for sample in samples.iter_mut() {
        gain += step;
        *sample *= gain;
        peak = peak.max(sample.abs());
    }
    peak
}

/// Adds `source`, scaled, into `destination`.
#[inline]
fn add_scaled(source: &[f32], destination: &mut [f32], gain: f32) {
    for (out, sample) in destination.iter_mut().zip(source) {
        *out += sample * gain;
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

/// Where a block lies on the timeline.
struct Block {
    start_beat: f64,
    beats_per_frame: f64,
    /// The transport's anchor in output frames from beat 0, and the frames
    /// played since. Their sum is the block's first frame. They are kept
    /// apart because the anchor has a fraction and the count does not: a
    /// position rounded from the anchor alone rounds the same way in every
    /// block, where one rounded from the sum could flip between blocks.
    anchor_frames: f64,
    elapsed: u64,
    sample_rate: f64,
}

/// Reads a region's audio for the frames of one block.
enum Reader<'a> {
    /// One frame of audio per output frame: frames are copied.
    Copy { left: &'a [f32], right: &'a [f32], first: i64 },
    /// Any other speed: linear interpolation.
    Interpolate { left: &'a [f32], right: &'a [f32], first: f64, step: f64 },
}

impl<'a> Reader<'a> {
    fn new(audio: &'a RegionAudio, block: &Block) -> Self {
        let frames = audio.data.frames();
        let (left, right) = (&audio.data.left[..frames], &audio.data.right[..frames]);
        if audio.step == 1.0 {
            // Snapping to whole frames moves the clip by at most half a
            // sample and plays the file exactly as it is stored. The sums
            // saturate: a clip with an absurd offset is silent, not a panic.
            let first = ((audio.origin + block.anchor_frames).round() as i64).saturating_add(block.elapsed as i64);
            Self::Copy { left, right, first }
        } else {
            let first = audio.origin + (block.anchor_frames + block.elapsed as f64) * audio.step;
            Self::Interpolate { left, right, first, step: audio.step }
        }
    }

    /// The audio for frame `i` of the block; silence outside the file.
    #[inline]
    fn at(&self, i: usize) -> (f32, f32) {
        match *self {
            Self::Copy { left, right, first } => match usize::try_from(first.saturating_add(i as i64)) {
                Ok(index) if index < left.len() => (left[index], right[index]),
                _ => (0.0, 0.0),
            },
            Self::Interpolate { left, right, first, step } => {
                let position = first + i as f64 * step;
                // Also catches positions that are not numbers at all.
                if !(0.0..left.len() as f64 - 1.0).contains(&position) {
                    return (0.0, 0.0);
                }
                let index = position as usize;
                let fraction = (position - index as f64) as f32;
                (
                    left[index] + (left[index + 1] - left[index]) * fraction,
                    right[index] + (right[index + 1] - right[index]) * fraction,
                )
            }
        }
    }
}

/// Mixes the audio clips of `track` into the block.
fn render_audio(track: &mut TrackNode, block: &Block, left: &mut [f32], right: &mut [f32]) {
    let frames = left.len();
    let end_beat = block.start_beat + frames as f64 * block.beats_per_frame;
    let guard = CLIP_FADE_SECONDS * block.sample_rate;
    let hand_over = (HAND_OVER_SECONDS * block.sample_rate) as u32;

    for region in &mut track.audio {
        if region.end <= block.start_beat || region.start >= end_beat {
            continue;
        }
        // Output frames between the clip start and this block (negative if
        // the clip starts inside the block).
        let elapsed = (block.start_beat - region.start) / block.beats_per_frame;
        let length = (region.end - region.start) / block.beats_per_frame;
        // The clip's own fades, never shorter than the click guard except
        // where the clip joins a neighbour and there is no edge to guard.
        let fade_in = (region.fade_in / block.beats_per_frame).max(if region.joins_previous { 0.0 } else { guard });
        let fade_out = (region.fade_out / block.beats_per_frame).max(if region.joins_next { 0.0 } else { guard });
        // Both edges are worked out the same way from a timeline position,
        // so that where one clip ends on the beat another starts on, the
        // frame at the seam belongs to exactly one of them.
        let frame_of = |beat: f64| ((beat - block.start_beat) / block.beats_per_frame).ceil().max(0.0) as usize;
        let first = frame_of(region.start);
        let last = frame_of(region.end).min(frames);

        let audio = Reader::new(&region.audio, block);
        let replaced = region.replaces.as_ref().map(|audio| Reader::new(audio, block));
        for i in first..last {
            let t = elapsed + i as f64;
            let (mut sample_left, mut sample_right) = audio.at(i);
            if let Some(replaced) = &replaced
                && region.blended < hand_over
            {
                let (old_left, old_right) = replaced.at(i);
                let mix = region.blended as f32 / hand_over as f32;
                sample_left = old_left + (sample_left - old_left) * mix;
                sample_right = old_right + (sample_right - old_right) * mix;
                region.blended += 1;
            }
            let rise = if fade_in > 0.0 { t / fade_in } else { 1.0 };
            let fall = if fade_out > 0.0 { (length - t) / fade_out } else { 1.0 };
            let gain = region.gain * rise.min(fall).clamp(0.0, 1.0) as f32;
            left[i] += sample_left * gain;
            right[i] += sample_right * gain;
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
            input_bus: None,
            output_bus: None,
            sends: Vec::new(),
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
                    _ => {}
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
