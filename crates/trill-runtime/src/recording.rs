//! What both front ends do the same way with recorded sound, and what the
//! program remembers of it from one run to the next: how long sound takes
//! to go round a pair of devices, as a measurement found it or as it was
//! put right by hand.
//!
//! That is the machine's and not a song's, and it is true of two devices
//! as they were opened: another buffer, another rate, or taken exclusively
//! is another figure. So it is kept in the settings under all of that
//! ([`RoundTrip`]), and the session is given the one that fits whenever
//! its devices change: by the housekeeping thread, within a moment. A
//! front end that sets one goes through here
//! ([`Runtime::keep_measured_latency`], [`Runtime::keep_latency_adjustment`]),
//! so that it is remembered.

use crate::Runtime;
use trill_engine::Session;
use trill_library::{RoundTrip, SaveError, Settings};

/// The folder beside the settings that holds the takes of songs that have
/// not been saved yet.
pub(crate) const RECORDINGS_FOLDER: &str = "recordings";

/// The devices that are open, as a round trip is kept for them: `None`
/// while no input is open, when there is nothing to go round.
fn pair(session: &Session) -> Option<RoundTrip> {
    let input = session.input_status().open?;
    let output = session.audio_info();
    Some(RoundTrip {
        output: output.device.clone().unwrap_or_default(),
        input: input.device,
        exclusive: output.exclusive,
        sample_rate: output.sample_rate.round() as u32,
        buffer_frames: output.buffer_frames,
        measured_ms: None,
        adjust_ms: 0.0,
    })
}

fn same_pair(one: &RoundTrip, other: &RoundTrip) -> bool {
    one.output == other.output
        && one.input == other.input
        && one.exclusive == other.exclusive
        && one.sample_rate == other.sample_rate
        && one.buffer_frames == other.buffer_frames
}

/// Gives the session what is kept for the devices it has open: what was
/// measured for them and what was added by hand, or neither.
pub(crate) fn apply_kept(session: &mut Session, kept: &[RoundTrip]) {
    let Some(now) = pair(session) else { return };
    let found = kept.iter().find(|kept| same_pair(kept, &now));
    let (measured, adjust) = found.map_or((None, 0.0), |kept| (kept.measured_ms, kept.adjust_ms));
    if session.latency_correction() != (measured, adjust) {
        session.set_measured_latency(measured);
        session.set_latency_adjustment(adjust);
    }
}

/// Puts `change` on what is kept for the devices `now`, and lets go of an
/// entry that says nothing any more.
fn keep(settings: &mut Settings, now: RoundTrip, change: impl FnOnce(&mut RoundTrip)) {
    let kept = &mut settings.audio.round_trips;
    let index = kept.iter().position(|kept| same_pair(kept, &now)).unwrap_or_else(|| {
        kept.push(now);
        kept.len() - 1
    });
    change(&mut kept[index]);
    if kept[index].measured_ms.is_none() && kept[index].adjust_ms == 0.0 {
        kept.remove(index);
    }
}

impl Runtime {
    /// Takes what a measurement of the round trip found
    /// ([`trill_engine::Measured::beyond_ms`]) for the devices that are
    /// open, in place of what they say, and remembers it for them; `None`
    /// goes back to what they say. Nothing is kept while no input is open.
    pub fn keep_measured_latency(&self, measured_ms: Option<f64>) -> Result<(), SaveError> {
        let now = {
            let mut session = self.session.lock();
            session.set_measured_latency(measured_ms);
            pair(&session)
        };
        match now {
            Some(now) => self.update_settings(|settings| keep(settings, now, |kept| kept.measured_ms = measured_ms)),
            None => Ok(()),
        }
    }

    /// Adds `adjust_ms` by hand to how long sound is taken to go round the
    /// devices that are open, and remembers it for them.
    pub fn keep_latency_adjustment(&self, adjust_ms: f64) -> Result<(), SaveError> {
        let now = {
            let mut session = self.session.lock();
            session.set_latency_adjustment(adjust_ms);
            pair(&session)
        };
        match now {
            Some(now) => self.update_settings(|settings| keep(settings, now, |kept| kept.adjust_ms = adjust_ms)),
            None => Ok(()),
        }
    }
}
