//! The session's part in MIDI controllers: the curves clips hold, moving a
//! controller by hand, what an instrument does with them, and noticing an
//! instrument that is given notes and stays silent.
//!
//! # The rules, in one place
//!
//! * **The clip under the playhead decides.** A controller is worth what
//!   the clip there has for it; after a clip ends, the pitch wheel, channel
//!   pressure and the pedals return to rest, and every other controller
//!   stays where the clip left it (`trill_core::controllers`).
//! * **It is in force wherever the song is.** Starting in the middle of a
//!   crescendo, seeking, or the loop coming round puts every controller
//!   where the song has it there, before the first note. While the song
//!   stands still, what stays put is where the song has it at the
//!   playhead, and the pedals and the wheel are at rest.
//! * **A controller moved by hand is the hand's** ([`Session::play_live`],
//!   [`Session::send_controller`], a MIDI port): the clip's curve for it
//!   is not played on that track until the song next starts playing, is
//!   moved while it plays, or loops round. Then the song has it again.
//! * **Nothing is left held or bent.** When the song stops, and at the end
//!   of a clip, the song's pedals come up and its pitch wheel centres;
//!   when a keyboard stops playing a track ([`Session::release_live`], a
//!   track disarmed or deselected), so do the pedals and the wheel the
//!   keyboard left there. A curve that is deleted while its pedal is down
//!   lets the pedal up.
//!
//! What a take does with controllers is in [`take`](super::take).

use super::{Session, SessionError};
use std::time::Duration;
use trill_core::device::ControllerSupport;
use trill_core::project::AutomationPoint;
use trill_core::{ClipId, ControllerKind, Device, DeviceId, TrackId};

/// How long notes must go without an answer before it is worth saying.
const UNANSWERED: Duration = Duration::from_millis(1000);

/// An instrument that is being given notes and puts nothing out; see
/// [`Session::silent_instrument`].
#[derive(Clone, Debug, PartialEq)]
pub struct SilentInstrument {
    pub device: DeviceId,
    /// The device's name, for saying which.
    pub name: String,
    /// Whether it is a plugin. A plugin that stays silent is most often one
    /// that waits for an expression or breath controller.
    pub plugin: bool,
    /// For how long notes have sounded without an answer.
    pub seconds: f64,
}

impl Session {
    // ---- curves in clips -----------------------------------------------

    /// The controllers a clip has curves for, in the order of
    /// [`ControllerKind`]: what a view lists without opening each one.
    /// Empty for an audio clip.
    pub fn clip_controllers(&self, clip: ClipId) -> Result<Vec<ControllerKind>, SessionError> {
        Ok(self.project.clip(clip)?.controllers().iter().map(|curve| curve.controller).collect())
    }

    /// The points of a clip's curve for `kind`; none if it has no such
    /// curve. Beats are measured from where the clip's content begins,
    /// like the starts of its notes (`Clip::content_beat` turns a place on
    /// the timeline into one), and values lie within
    /// [`ControllerKind::range`]. `ControllerCurve::value_at` is what the
    /// curve is worth anywhere, and `ControllerCurve::played` its points
    /// as they are played.
    pub fn controller_curve(&self, clip: ClipId, kind: ControllerKind) -> Result<Vec<AutomationPoint>, SessionError> {
        Ok(self.project.clip(clip)?.controller(kind).map(|curve| curve.points.clone()).unwrap_or_default())
    }

    /// Replaces a clip's curve for `kind`; no points removes it. One undo
    /// step, or part of one: between [`Session::begin_gesture`] and
    /// [`Session::end_gesture`] every call belongs to the same step, which
    /// is how a point is dragged or a curve drawn with the pencil. Fails,
    /// changing nothing, if a value is out of range, a beat is negative or
    /// the clip is an audio clip.
    pub fn set_controller_curve(
        &mut self,
        clip: ClipId,
        kind: ControllerKind,
        points: Vec<AutomationPoint>,
    ) -> Result<(), SessionError> {
        self.edit(|project| project.clip_mut(clip)?.set_controller(kind, points))
    }

    // ---- by hand ---------------------------------------------------------

    /// Moves a controller of a track's instrument now, whether or not the
    /// track is armed, as [`Session::note_on`] plays a note there. `value`
    /// is within [`ControllerKind::range`] and is kept to it. The
    /// controller is the hand's from then on (see the module
    /// documentation), and while a take is recorded on the track the
    /// movement is recorded.
    ///
    /// To move a controller wherever a keyboard plays, give
    /// [`Session::play_live`] the event `ControllerKind::event` makes.
    pub fn send_controller(&mut self, track: TrackId, kind: ControllerKind, value: f64) {
        let (low, high) = kind.range();
        let value = if value.is_finite() { value.clamp(low, high) } else { low.max(0.0) };
        let now = self.engine.now_ns();
        self.engine.live().play_on_at(now, track, kind.event(value as f32));
    }

    // ---- instruments -----------------------------------------------------

    /// The device a track's notes and controllers are played through: the
    /// first device of its chain that is switched on and is not one of the
    /// built-in effects.
    pub fn instrument(&self, track: TrackId) -> Option<DeviceId> {
        self.instrument_device(track).map(|device| device.id)
    }

    pub(super) fn instrument_device(&self, track: TrackId) -> Option<&Device> {
        let track = self.project.track(track).ok()?;
        track.devices.iter().find(|device| device.enabled && trill_dsp::is_instrument(&device.uri) != Some(false))
    }

    /// Which controllers the instrument of a track does something with, as
    /// far as that can be known: all of them for a built-in instrument,
    /// the ones a VST3 plugin names a parameter for, and "any" for a CLAP
    /// plugin that takes MIDI (it is handed every controller and does not
    /// say which it uses). [`ControllerSupport::Unknown`] for a track
    /// without an instrument. Cheap enough to ask whenever a view is drawn.
    pub fn controller_support(&self, track: TrackId) -> ControllerSupport {
        let Some(device) = self.instrument_device(track) else { return ControllerSupport::Unknown };
        match trill_dsp::controller_support(&device.uri) {
            ControllerSupport::Unknown => {
                let meta = self.bank.devices.get(&device.id);
                let controller = meta.and_then(|meta| meta.controller.as_ref());
                controller.map_or(ControllerSupport::Unknown, |controller| controller.lock().controller_support())
            }
            known => known,
        }
    }

    /// The instrument of a track, if it has been given notes for a second
    /// or more and nothing has come out of the track's devices in answer.
    /// `None` as soon as anything is heard, and for a track whose
    /// instrument failed to load ([`Session::problems`] says that).
    ///
    /// This is known while the song plays and while keys are held, at the
    /// cost of one look at the track's output per audio buffer, and only
    /// until the first sound of a phrase: an instrument that rings out
    /// under a long note has answered. Cheap to ask on every frame.
    pub fn silent_instrument(&self, track: TrackId) -> Option<SilentInstrument> {
        let frames = self.meters.tracks.get(&track)?.unanswered_frames();
        let seconds = f64::from(frames) / self.engine.sample_rate().max(1.0);
        if seconds < UNANSWERED.as_secs_f64() {
            return None;
        }
        let device = self.instrument_device(track)?;
        let failed = self.bank.devices.get(&device.id).is_none_or(|meta| meta.error.is_some());
        (!failed).then(|| SilentInstrument {
            device: device.id,
            name: device.name.clone(),
            plugin: trill_dsp::is_instrument(&device.uri).is_none(),
            seconds,
        })
    }
}
