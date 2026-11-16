//! The session's part in playing by hand: which audio device sounds, where
//! what is played goes, and recording it.
//!
//! # Where what is played goes
//!
//! Tracks can be **armed**. A keyboard ([`Session::play_live`], and every
//! MIDI port through [`Session::live`]) plays the instruments of the armed
//! tracks, or of the **selected** track when none is armed, which the
//! window keeps the session told of ([`Session::set_selected_track`]).
//! [`Session::note_on`] and [`Session::note_off`] name their track and
//! play it whatever is armed. Neither arming nor the selection is part of
//! the project: they are not saved with it and are not undo steps.
//!
//! # Recording
//!
//! [`Session::record`] starts a take on the armed tracks from where the
//! song is, after a count-in if the song is stopped; [`Session::stop`]
//! ends it. What is played on those tracks meanwhile, by a keyboard or
//! through `note_on`, is written into clips as it is played (see
//! [`take`](super::take) for exactly where), at the positions it was
//! heard: the engine takes off the time the sound needs to reach the
//! speakers. A take is one undo step, along with anything else that was
//! changed while it ran.

use super::take::{Take, TakeClip};
use super::{Session, SessionError, UndoGroup};
use crate::compile::StandIns;
use crate::engine::{Applied, Health, Message, RecordPhase};
use crate::host::{AudioConfig, AudioDevices, StreamFailure};
use crate::live::{LiveSender, MAX_LIVE_TRACKS, TrackSet};
use std::time::{Duration, Instant};
use trill_core::device::EventKind;
use trill_core::{TrackId, TrackKind};

/// The longest count-in, in bars.
pub const MAX_COUNT_IN_BARS: u32 = 8;
/// How long stopping waits for the audio thread to say where the take
/// ended, before ending it where the song was last known to be.
const STOP_WAIT: Duration = Duration::from_millis(400);

/// Whether a take is being recorded.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RecordState {
    /// Nothing is being recorded.
    Idle,
    /// The count-in is clicking; the song has not started.
    CountIn,
    /// The song is rolling and what is played is being written down.
    Recording,
}

/// Recording as it stands; see [`Session::record_status`].
#[derive(Clone, Debug, PartialEq)]
pub struct RecordStatus {
    pub state: RecordState,
    /// The tracks being recorded.
    pub tracks: Vec<TrackId>,
    /// Where the take began, once the song is rolling.
    pub start_beat: Option<f64>,
    /// How many times the loop has come round during the take.
    pub pass: u32,
    /// How many notes have been played to the end so far.
    pub notes: usize,
    /// The clips written into so far.
    pub clips: Vec<TakeClip>,
}

/// A take between [`Session::record`] and its end.
pub(super) struct ActiveTake {
    take: Take,
    /// Everything the take writes is one undo step.
    group: UndoGroup,
}

/// What the session keeps about playing by hand.
#[derive(Default)]
pub(super) struct Playing {
    armed: Vec<TrackId>,
    selected: Option<TrackId>,
    /// The tracks the audio thread was last told a keyboard plays.
    sent: Option<TrackSet>,
    count_in_bars: u32,
    take: Option<ActiveTake>,
    /// The number of the last take started.
    takes: u32,
    last_take: Vec<TakeClip>,
    /// Bumped whenever the audio stream changes.
    audio_revision: u64,
}

impl Playing {
    pub fn new() -> Self {
        Self { count_in_bars: 1, ..Self::default() }
    }
}

impl Session {
    // ---- the audio device ----------------------------------------------

    /// What was asked of the audio hardware. [`Session::audio_info`] is
    /// what came of it.
    pub fn audio_config(&self) -> &AudioConfig {
        self.engine.config()
    }

    /// The audio devices that could be chosen. This asks the system, which
    /// takes a moment (and with a driver that does not answer, up to a few
    /// seconds, after which the lists come back empty): call it when a
    /// settings panel opens, not on every frame.
    pub fn audio_devices(&mut self) -> AudioDevices {
        self.engine.devices()
    }

    /// Asks for a different audio stream (another device, sample rate or
    /// buffer size, or the device to itself) and opens it now, while the
    /// program runs. Returns nothing because it cannot fail: if what was
    /// asked for cannot be had, the nearest thing is opened, down to no
    /// device at all, and [`Session::audio_info`] says what is in use and,
    /// in its `warning`, why. The project, its undo history and the
    /// playhead are untouched.
    ///
    /// The sound stops for the moment it takes (a fraction of a second; a
    /// few seconds if a device does not answer) and the caller waits that
    /// long. Playback carries on if the sample rate is the same. If the
    /// rate changes, every device is created again from the project at the
    /// new rate: playback stops, tails are cut, and the plugin editors
    /// that were open close.
    pub fn set_audio_config(&mut self, config: AudioConfig) {
        self.before_audio_change();
        let position = self.engine.position();
        let applied = self.engine.apply(config);
        self.after_audio_change(applied, position);
    }

    /// A number that changes whenever the audio stream does: when
    /// [`Session::set_audio_config`] is called, when a device is lost and
    /// another takes over, when a lost device comes back. A view that
    /// shows [`Session::audio_info`] reads it again when this moves.
    pub fn audio_revision(&self) -> u64 {
        self.playing.audio_revision
    }

    /// How long after the engine renders a frame it is heard, in
    /// milliseconds, as the device last reported: its buffer and whatever
    /// lies between the buffer and the speaker. It moves a little while a
    /// stream runs.
    pub fn output_latency_ms(&self) -> f64 {
        let rate = self.engine.sample_rate();
        if rate > 0.0 { self.engine.output_latency() as f64 * 1000.0 / rate } else { 0.0 }
    }

    /// What the stream is about to go through: a take ends, and plugins
    /// hand over their state in case they have to be created again.
    fn before_audio_change(&mut self) {
        if self.playing.take.is_some() {
            self.engine.send(Message::Stop);
            self.finish_take(true);
        }
        self.absorb_editor_edits();
        self.capture_device_state();
    }

    fn after_audio_change(&mut self, applied: Applied, position: f64) {
        self.playing.audio_revision += 1;
        self.audio.media().set_loader_rate(Some(self.engine.sample_rate()));
        if applied.reset {
            // The audio thread starts from nothing: every device is made
            // anew at the new rate, and it is told again what it knew.
            self.bank.devices.clear();
            self.stand_ins = StandIns::default();
            self.compile_and_swap();
            self.engine.send(Message::Seek(position));
            self.engine.send(Message::Metronome(self.metronome));
            self.playing.sent = None;
            self.send_live_targets();
        } else {
            // Keys that came up while there was no stream were not heard
            // to: nothing may be left sounding.
            self.engine.send(Message::ReleaseEverything);
        }
    }

    /// Looks after the audio stream: a device that was lost is replaced by
    /// the next best, and one that has come back is taken up again. Part
    /// of [`Session::housekeeping`].
    pub(super) fn tend_audio(&mut self) {
        let reason = match self.engine.check() {
            Health::Good => return,
            Health::Retry => None,
            Health::Failed(failure) => Some(match failure {
                StreamFailure::DeviceLost => "the audio device was disconnected",
                StreamFailure::Failed => "the audio device stopped working",
                StreamFailure::Stalled => "the audio device stopped responding",
            }),
        };
        let was = self.engine.info().output.clone();
        if let Some(reason) = reason {
            log::warn!("{reason} ({was}); looking for another");
        }
        self.before_audio_change();
        let position = self.engine.position();
        let applied = self.engine.reopen();
        self.after_audio_change(applied, position);
        if let Some(reason) = reason {
            self.engine.note_failure(&format!("{reason} ({was})"));
        }
    }

    // ---- where what is played goes -------------------------------------

    /// The way in for live input from other threads: a MIDI port's driver
    /// thread sends what is played straight to the audio thread through
    /// this, without taking the session. Sending never waits and never
    /// allocates. What is sent plays where a keyboard plays (see the
    /// module documentation) and is recorded like anything else.
    pub fn live(&self) -> LiveSender {
        self.engine.live().clone()
    }

    /// Arms a track, so that a keyboard plays it and [`Session::record`]
    /// records it, or disarms it. Disarming lets go of whatever the
    /// keyboard was holding on the track. Only tracks that hold clips can
    /// be armed (not groups or returns), and at most
    /// [`MAX_LIVE_TRACKS`](crate::live::MAX_LIVE_TRACKS) at once.
    pub fn set_armed(&mut self, track: TrackId, armed: bool) -> Result<(), SessionError> {
        let found = self.project.track(track)?;
        if !armed {
            self.playing.armed.retain(|id| *id != track);
        } else if !self.playing.armed.contains(&track) {
            if found.kind != TrackKind::Normal {
                let kind = if found.kind == TrackKind::Group { "group" } else { "return" };
                return Err(SessionError::Recording(format!(
                    "{track} ({}) is a {kind} track and cannot be armed; arm a track that holds clips",
                    found.name
                )));
            }
            if self.playing.armed.len() >= MAX_LIVE_TRACKS {
                return Err(SessionError::Recording(format!(
                    "{MAX_LIVE_TRACKS} tracks are armed already, which is the most; disarm one first"
                )));
            }
            self.playing.armed.push(track);
        }
        self.send_live_targets();
        Ok(())
    }

    /// The armed tracks, in the order they were armed.
    pub fn armed(&self) -> &[TrackId] {
        &self.playing.armed
    }

    pub fn is_armed(&self, track: TrackId) -> bool {
        self.playing.armed.contains(&track)
    }

    /// Tells the session which track is selected in the window: the one a
    /// keyboard plays while no track is armed. `None`, or a track that
    /// holds no clips, means none.
    pub fn set_selected_track(&mut self, track: Option<TrackId>) {
        self.playing.selected = track;
        self.send_live_targets();
    }

    pub fn selected_track(&self) -> Option<TrackId> {
        self.playing.selected
    }

    /// The tracks a keyboard plays right now: the armed ones, or the
    /// selected one when none is armed. Empty when what is played would
    /// not be heard.
    pub fn live_tracks(&self) -> Vec<TrackId> {
        let normal = |track: &TrackId| self.project.track(*track).is_ok_and(|found| found.kind == TrackKind::Normal);
        let armed: Vec<TrackId> = self.playing.armed.iter().copied().filter(normal).collect();
        if armed.is_empty() { self.playing.selected.filter(normal).into_iter().collect() } else { armed }
    }

    /// Plays `kind` now, as a key or a control of a keyboard would: on the
    /// tracks [`Session::live_tracks`] names. For input that arrives on
    /// another thread, [`Session::live`] does the same without the
    /// session.
    pub fn play_live(&mut self, kind: EventKind) {
        let now = self.engine.now_ns();
        self.engine.live().play_at(now, kind);
    }

    /// Lets go of everything keyboards are holding: every key, the sustain
    /// pedal and the pitch wheel, on every track. For when a keyboard can
    /// no longer do so itself: its port has gone. The engine does not
    /// know which keyboard a key came from, so the keys of every keyboard
    /// are let go of, not only the one that went: one more than need be
    /// rather than one too few. Notes started with [`Session::note_on`]
    /// are not touched.
    pub fn release_live(&mut self) {
        self.engine.send(Message::ReleaseLive);
    }

    /// Brings the audio thread in line with what is armed and selected.
    /// Tracks that are gone are forgotten.
    pub(super) fn send_live_targets(&mut self) {
        let project = &self.project;
        self.playing.armed.retain(|track| project.track(*track).is_ok());
        if self.playing.selected.is_some_and(|track| project.track(track).is_err()) {
            self.playing.selected = None;
        }
        let targets = TrackSet::of(&self.live_tracks());
        if self.playing.sent != Some(targets) {
            self.playing.sent = Some(targets);
            self.engine.send(Message::LiveTargets(targets));
        }
    }

    // ---- recording -----------------------------------------------------

    /// How many bars of count-in come before a take that starts from a
    /// stopped song; 0 for none. One bar unless changed.
    pub fn count_in(&self) -> u32 {
        self.playing.count_in_bars
    }

    /// Sets the count-in, in bars, up to [`MAX_COUNT_IN_BARS`].
    pub fn set_count_in(&mut self, bars: u32) {
        self.playing.count_in_bars = bars.min(MAX_COUNT_IN_BARS);
    }

    /// Starts recording a take on the armed tracks, from where the song
    /// is. If the song is stopped, the metronome first counts in
    /// ([`Session::count_in`]) and then the song starts; if it is playing,
    /// recording begins at once. The count-in clicks whether or not the
    /// metronome is on; during the take the metronome does as it is set.
    ///
    /// The take ends with [`Session::stop`]. Fails if no track is armed.
    /// Does nothing if a take is already being recorded.
    pub fn record(&mut self) -> Result<(), SessionError> {
        if self.playing.take.is_some() {
            return Ok(());
        }
        self.send_live_targets();
        let tracks: Vec<TrackId> = self
            .playing
            .armed
            .iter()
            .copied()
            .filter(|track| self.project.track(*track).is_ok_and(|found| found.kind == TrackKind::Normal))
            .collect();
        if tracks.is_empty() {
            return Err(SessionError::Recording(
                "no track is armed, so there is nothing to record on; arm a track first".to_string(),
            ));
        }
        // What an earlier take left unread is not this one's.
        while self.engine.next_captured().is_some() {}

        let signature = self.project.time_signature;
        let bar = signature.beats_per_bar();
        let region = self.project.loop_region;
        let loop_region = region.enabled.then_some((region.start, region.end));
        self.playing.takes = self.playing.takes.wrapping_add(1);
        let id = self.playing.takes;
        let group = self.begin_group();
        self.playing.take = Some(ActiveTake { take: Take::new(id, &tracks, loop_region, bar), group });
        self.playing.last_take.clear();
        self.engine.send(Message::Record {
            take: id,
            tracks: TrackSet::of(&tracks),
            count_in_beats: self.playing.count_in_bars as f64 * bar,
        });
        Ok(())
    }

    /// Whether a take is being recorded, and how far it has got.
    pub fn record_status(&self) -> RecordStatus {
        let Some(active) = &self.playing.take else {
            return RecordStatus {
                state: RecordState::Idle,
                tracks: Vec::new(),
                start_beat: None,
                pass: 0,
                notes: 0,
                clips: Vec::new(),
            };
        };
        let take = &active.take;
        // The audio thread knows first; the take learns of it at the next
        // round of housekeeping.
        let rolling = take.start().is_some() || self.engine.record_phase() == RecordPhase::Rolling;
        RecordStatus {
            state: if rolling { RecordState::Recording } else { RecordState::CountIn },
            tracks: take.tracks(),
            start_beat: take.start(),
            pass: take.pass(),
            notes: take.note_count(),
            clips: take.clips(),
        }
    }

    /// Whether a take is being recorded or counted in.
    pub fn is_recording(&self) -> bool {
        self.playing.take.is_some()
    }

    /// The clips the last take wrote into: empty until a take has ended,
    /// and if nothing was played in it.
    pub fn last_take(&self) -> &[TakeClip] {
        &self.playing.last_take
    }

    /// Takes in what the audio thread has reported about the take and
    /// writes the notes that were finished into the project. Part of
    /// [`Session::housekeeping`], so a clip grows while it is recorded.
    pub(super) fn tend_take(&mut self) {
        if self.playing.take.is_none() {
            // Nobody is listening; keep the queue from filling up.
            while self.engine.next_captured().is_some() {}
            return;
        }
        self.read_take();
        // The clips keep up with the song while nothing is played.
        let heard = self.engine.position() - self.latency_beats();
        if let Some(active) = &mut self.playing.take {
            active.take.heard(heard);
        }
        self.write_take();
        if self.playing.take.as_ref().is_some_and(|active| active.take.is_stopped()) {
            self.finish_take(false);
        }
    }

    /// The output latency in beats at the project's tempo.
    fn latency_beats(&self) -> f64 {
        self.output_latency_ms() / 1000.0 * self.project.tempo / 60.0
    }

    fn read_take(&mut self) {
        let Some(active) = &mut self.playing.take else { return };
        while let Some(captured) = self.engine.next_captured() {
            active.take.handle(captured);
        }
    }

    fn write_take(&mut self) {
        let Some(mut active) = self.playing.take.take() else { return };
        if active.take.has_news(&self.project) {
            if let Err(error) = self.edit(|project| active.take.write(project)) {
                log::error!("could not write what was recorded: {error}");
            }
        }
        self.playing.take = Some(active);
    }

    /// Ends the take: what is held ends, what was played is in the
    /// project, and the whole is one undo step. With `wait`, the audio
    /// thread is given a moment to say exactly where the take stopped.
    pub(super) fn finish_take(&mut self, wait: bool) {
        if self.playing.take.is_none() {
            return;
        }
        self.read_take();
        if wait {
            let deadline = Instant::now() + STOP_WAIT;
            while self.playing.take.as_ref().is_some_and(|active| !active.take.is_stopped())
                && Instant::now() < deadline
            {
                self.engine.pump();
                std::thread::sleep(Duration::from_millis(1));
                self.read_take();
            }
        }
        // The audio thread never said: end the take where the song was
        // last known to be.
        let position = self.engine.position() - self.latency_beats();
        let Some(mut active) = self.playing.take.take() else { return };
        if !active.take.is_stopped() {
            let pass = active.take.pass();
            active.take.stop_at(position, pass);
        }
        self.playing.take = Some(active);
        self.write_take();
        if let Some(active) = self.playing.take.take() {
            self.playing.last_take = active.take.clips();
            self.end_group(active.group);
        }
    }

    /// Drops a take without writing it: the project it belonged to is
    /// being replaced.
    pub(super) fn abandon_take(&mut self) {
        if let Some(active) = self.playing.take.take() {
            self.end_group(active.group);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::AudioBackend;
    use std::sync::Arc;
    use trill_core::ChainRef;
    use trill_dsp::BuiltinFactory;

    fn session() -> Session {
        Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: 48_000 }).unwrap()
    }

    #[test]
    fn a_keyboard_plays_the_armed_tracks_or_else_the_selected_one() {
        let mut session = session();
        let keys = session.edit(|p| Ok(p.add_track("Keys"))).unwrap();
        let bass = session.edit(|p| Ok(p.add_track("Bass"))).unwrap();
        let group = session.edit(|p| Ok(p.add_track_of_kind("Group", TrackKind::Group))).unwrap();
        assert!(session.live_tracks().is_empty(), "nothing armed, nothing selected");

        session.set_selected_track(Some(keys));
        assert_eq!(session.live_tracks(), [keys]);
        session.set_selected_track(Some(group));
        assert!(session.live_tracks().is_empty(), "a group has no instrument of its own to play");

        session.set_armed(bass, true).unwrap();
        session.set_armed(bass, true).unwrap();
        assert_eq!(session.armed(), [bass]);
        session.set_selected_track(Some(keys));
        assert_eq!(session.live_tracks(), [bass], "armed tracks come before the selection");
        session.set_armed(keys, true).unwrap();
        assert_eq!(session.live_tracks(), [bass, keys]);
        assert!(session.is_armed(keys));

        session.set_armed(bass, false).unwrap();
        session.set_armed(keys, false).unwrap();
        assert_eq!(session.live_tracks(), [keys], "back to the selection");
    }

    #[test]
    fn arming_says_what_is_wrong() {
        let mut session = session();
        let group = session.edit(|p| Ok(p.add_track_of_kind("Drums", TrackKind::Group))).unwrap();
        let error = session.set_armed(group, true).unwrap_err().to_string();
        assert!(error.contains("group track") && error.contains("Drums"), "{error}");
        assert!(session.set_armed(TrackId(999), true).is_err());
        assert!(session.armed().is_empty());

        let tracks: Vec<TrackId> = (0..MAX_LIVE_TRACKS + 1)
            .map(|index| session.edit(|p| Ok(p.add_track(format!("T{index}")))).unwrap())
            .collect();
        for track in &tracks[..MAX_LIVE_TRACKS] {
            session.set_armed(*track, true).unwrap();
        }
        let error = session.set_armed(tracks[MAX_LIVE_TRACKS], true).unwrap_err().to_string();
        assert!(error.contains("disarm one first"), "{error}");

        let error = session.record_without_armed_tracks_for_test();
        assert!(error.contains("arm a track first"), "{error}");
    }

    #[test]
    fn tracks_that_are_deleted_are_no_longer_armed_or_selected() {
        let mut session = session();
        let keys = session.edit(|p| Ok(p.add_track("Keys"))).unwrap();
        let bass = session.edit(|p| Ok(p.add_track("Bass"))).unwrap();
        session.add_device(ChainRef::Track(keys), "builtin:polysynth", None, None).unwrap();
        session.set_armed(keys, true).unwrap();
        session.set_selected_track(Some(bass));

        session.edit(|p| p.remove_track(keys).map(|_| ())).unwrap();
        assert!(session.armed().is_empty());
        assert_eq!(session.live_tracks(), [bass]);
        session.edit(|p| p.remove_track(bass).map(|_| ())).unwrap();
        assert_eq!(session.selected_track(), None);
        assert!(session.live_tracks().is_empty());

        // Undoing the deletion brings the track back, not its arming.
        assert!(session.undo() && session.undo());
        assert!(session.project().track(keys).is_ok());
        assert!(session.armed().is_empty());
    }

    #[test]
    fn the_count_in_has_a_limit() {
        let mut session = session();
        assert_eq!(session.count_in(), 1);
        session.set_count_in(0);
        assert_eq!(session.count_in(), 0);
        session.set_count_in(100);
        assert_eq!(session.count_in(), MAX_COUNT_IN_BARS);
    }

    impl Session {
        /// Disarms everything and asks to record: the refusal, as text.
        fn record_without_armed_tracks_for_test(&mut self) -> String {
            for track in self.armed().to_vec() {
                self.set_armed(track, false).unwrap();
            }
            self.record().unwrap_err().to_string()
        }
    }
}
