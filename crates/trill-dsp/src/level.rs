//! The level MIDI gives a whole instrument: channel volume (controller 7)
//! times expression (controller 11).
//!
//! Every built-in instrument follows both, the way General MIDI lays down:
//! the gain is the square of the controller's position, so half way is a
//! quarter of the amplitude (-12 dB) and zero is silence. Neither has been
//! moved until a controller says so, and then both are at the top: an
//! instrument that is never sent either plays exactly as it did before
//! there were controllers.
//!
//! A level never jumps while something sounds. It closes on where it was
//! put a little with every frame: fast enough to follow any swell a few
//! milliseconds behind, slow enough that a step in a controller is not a
//! click and that a curve sent as a value every few frames is heard as the
//! slope it is. The journey is worked out frame by frame, from nothing but
//! where the level is and where it was put, so that the same controllers
//! make the same sound however the frames are divided into buffers.

use trill_core::controllers::{CONTROLLER_EXPRESSION, CONTROLLER_VOLUME, ControllerKind};
use trill_core::device::ControllerSupport;

/// How long the level takes to cover most of the way (all but 1/e of it)
/// to where it was put, in seconds.
const SMOOTHING_SECONDS: f32 = 0.003;
/// Closer than this, it is there: one part in a hundred thousand of full
/// level, 100 dB down.
const ARRIVED: f32 = 1e-5;

#[derive(Copy, Clone, Debug)]
pub(crate) struct ChannelLevel {
    volume: f32,
    expression: f32,
    /// The gain both come to, and where the gain heard is on its way there.
    target: f32,
    current: f32,
}

impl ChannelLevel {
    pub(crate) fn new() -> Self {
        Self { volume: 1.0, expression: 1.0, target: 1.0, current: 1.0 }
    }

    /// Takes a controller event. Returns whether it was volume or
    /// expression. While nothing is `sounding` there is no click to avoid,
    /// and the level is where it was put at once: the first note of a song
    /// started in the middle of a crescendo begins at the level it has
    /// there.
    pub(crate) fn control(&mut self, controller: u8, value: f32, sounding: bool) -> bool {
        let position = if value.is_finite() { value.clamp(0.0, 1.0) } else { return controller_is_level(controller) };
        match controller {
            CONTROLLER_VOLUME => self.volume = position,
            CONTROLLER_EXPRESSION => self.expression = position,
            _ => return false,
        }
        self.target = (self.volume * self.volume) * (self.expression * self.expression);
        if !sounding {
            self.current = self.target;
        }
        true
    }

    /// The gain for the next frame. Exactly 1 for an instrument whose
    /// level was never moved.
    #[inline]
    pub(crate) fn next(&mut self, sample_rate: f32) -> f32 {
        if self.current != self.target {
            let wanted = self.target - self.current;
            if wanted.abs() < ARRIVED {
                self.current = self.target;
            } else {
                self.current += wanted * (1.0 / (SMOOTHING_SECONDS * sample_rate.max(1.0))).min(1.0);
            }
        }
        self.current
    }

    /// The gain for each of the next `gains.len()` frames.
    pub(crate) fn fill(&mut self, gains: &mut [f32], sample_rate: f32) {
        if self.current == self.target {
            gains.fill(self.current);
        } else {
            for gain in gains {
                *gain = self.next(sample_rate);
            }
        }
    }

    /// Nothing sounds any more: the level is where it was put.
    pub(crate) fn settle(&mut self) {
        self.current = self.target;
    }
}

fn controller_is_level(controller: u8) -> bool {
    matches!(controller, CONTROLLER_VOLUME | CONTROLLER_EXPRESSION)
}

/// Whether the built-in device `uri` is an instrument (`Some(true)`) or an
/// effect (`Some(false)`); `None` for a device that is not built in.
pub fn is_instrument(uri: &str) -> Option<bool> {
    match controller_support(uri) {
        ControllerSupport::Listed(kinds) => Some(!kinds.is_empty()),
        _ => None,
    }
}

/// The controllers the built-in device `uri` does something with: all of
/// them are known. A device that is not built in gets
/// [`ControllerSupport::Unknown`].
pub fn controller_support(uri: &str) -> ControllerSupport {
    let level = [ControllerKind::VOLUME, ControllerKind::EXPRESSION];
    let kinds = match uri {
        "builtin:polysynth" => {
            vec![ControllerKind::MOD_WHEEL, level[0], level[1], ControllerKind::SUSTAIN, ControllerKind::PitchBend]
        }
        "builtin:sampler" => vec![level[0], level[1], ControllerKind::SUSTAIN, ControllerKind::PitchBend],
        "builtin:drumkit" | "builtin:drumrack" => level.to_vec(),
        uri if uri.starts_with("builtin:") => Vec::new(),
        _ => return ControllerSupport::Unknown,
    };
    ControllerSupport::Listed(kinds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untouched_level_is_exactly_one() {
        let mut level = ChannelLevel::new();
        let mut gains = [0.0f32; 64];
        level.fill(&mut gains, 48_000.0);
        assert_eq!(gains, [1.0; 64]);
        assert_eq!(level.next(48_000.0), 1.0);
        // Other controllers are not its business.
        assert!(!level.control(1, 0.5, true) && !level.control(64, 1.0, true));
        assert_eq!(level.next(48_000.0), 1.0);
    }

    #[test]
    fn volume_and_expression_multiply_and_follow_a_square_law() {
        let mut level = ChannelLevel::new();
        assert!(level.control(CONTROLLER_EXPRESSION, 0.5, false));
        assert_eq!(level.next(48_000.0), 0.25, "half way is a quarter of the amplitude, at once in silence");
        assert!(level.control(CONTROLLER_VOLUME, 0.5, false));
        assert_eq!(level.next(48_000.0), 0.0625);
        level.control(CONTROLLER_EXPRESSION, 0.0, false);
        assert_eq!(level.next(48_000.0), 0.0, "no expression, no sound");
        // Nonsense from a broken controller changes nothing.
        level.control(CONTROLLER_EXPRESSION, f32::NAN, false);
        assert_eq!(level.next(48_000.0), 0.0);
        level.control(CONTROLLER_EXPRESSION, 7.0, false);
        level.control(CONTROLLER_VOLUME, 7.0, false);
        assert_eq!(level.next(48_000.0), 1.0, "beyond the top is the top");
    }

    #[test]
    fn while_something_sounds_the_level_travels_instead_of_jumping() {
        let mut level = ChannelLevel::new();
        level.control(CONTROLLER_EXPRESSION, 0.0, true);
        // Most of the way in three milliseconds (144 frames at 48 kHz), all
        // of it within forty, and never a jump.
        let mut down = [0.0f32; 1_920];
        level.fill(&mut down, 48_000.0);
        assert!((down[0] - (1.0 - 1.0 / 144.0)).abs() < 1e-6, "{:?}", &down[..4]);
        assert!((down[143] - (-1.0f32).exp()).abs() < 0.01, "{}", down[143]);
        assert!(down.windows(2).all(|pair| pair[1] <= pair[0] && pair[0] - pair[1] < 0.007));
        assert_eq!((down[1_919], level.next(48_000.0)), (0.0, 0.0), "arrived");
        // However the frames are divided up, it is the same journey.
        level.control(CONTROLLER_EXPRESSION, 1.0, true);
        let mut divided = level;
        let whole: Vec<f32> = (0..1_920).map(|_| level.next(48_000.0)).collect();
        let mut pieces = vec![0.0f32; 1_920];
        let (first, rest) = pieces.split_at_mut(37);
        divided.fill(first, 48_000.0);
        let (second, third) = rest.split_at_mut(1);
        divided.fill(second, 48_000.0);
        divided.fill(third, 48_000.0);
        assert_eq!(pieces, whole);
        assert_eq!(whole[1_919], 1.0);
        // And when the sound has stopped there is nothing to travel for.
        level.control(CONTROLLER_EXPRESSION, 0.5, true);
        level.settle();
        assert_eq!(level.next(48_000.0), 0.25);
    }

    #[test]
    fn what_each_built_in_device_responds_to_is_known() {
        let synth = controller_support("builtin:polysynth");
        assert_eq!(synth.responds_to(ControllerKind::EXPRESSION), Some(true));
        assert_eq!(synth.responds_to(ControllerKind::MOD_WHEEL), Some(true));
        assert_eq!(synth.responds_to(ControllerKind::BREATH), Some(false));
        assert_eq!(controller_support("builtin:sampler").responds_to(ControllerKind::MOD_WHEEL), Some(false));
        assert_eq!(controller_support("builtin:drumkit").responds_to(ControllerKind::EXPRESSION), Some(true));
        assert_eq!(controller_support("builtin:reverb"), ControllerSupport::Listed(Vec::new()));
        assert_eq!(controller_support("clap:whatever"), ControllerSupport::Unknown);
        assert_eq!(
            (is_instrument("builtin:sampler"), is_instrument("builtin:eq"), is_instrument("vst3:x")),
            (Some(true), Some(false), None)
        );
    }
}
