//! What lights when: how brightly a note glows at a moment, and how hard
//! the picture was just hit.
//!
//! These come from the song's own notes, not from what a meter shows, so
//! they land exactly: a note is at its brightest on the frame it starts
//! on, and a kick moves the picture on the kick. They are functions of the
//! time alone, so a frame can be asked for at any moment (a film asks for
//! several inside one frame, for its motion blur).

use crate::score::{Hit, Time};
use trill_core::{Beats, Note};

/// Seconds a struck note takes to fall from its flash to its held glow.
const FLASH_FALL: f64 = 0.16;
/// How brightly a held note glows against its flash.
const HELD: f32 = 0.38;
/// Seconds a note's glow takes to fall away once it has ended.
const RELEASE_FALL: f64 = 0.09;
/// How much of the glow the softest note has.
const SOFTEST: f32 = 0.45;

/// Seconds the pulse of a kick, the tear of a snare and the flare of a
/// crash take to fall away.
pub const KICK_FALL: f64 = 0.10;
pub const SNARE_FALL: f64 = 0.07;
pub const CRASH_FALL: f64 = 0.9;

/// How brightly `note` glows at `beat`, 0..=1: nothing before it starts,
/// all it has when it does, less while it is held and gone a moment after
/// it ends. A hard note glows more than a soft one.
pub fn glow(note: &Note, beat: Beats, time: &Time) -> f32 {
    let seconds = time.between(note.start, beat);
    if seconds < 0.0 {
        return 0.0;
    }
    let held_for = time.over(note.start, note.length);
    let while_held = |seconds: f64| HELD + (1.0 - HELD) * (-seconds / FLASH_FALL).exp() as f32;
    let level = match seconds <= held_for {
        true => while_held(seconds),
        false => while_held(held_for) * (-(seconds - held_for) / RELEASE_FALL).exp() as f32,
    };
    level * (SOFTEST + (1.0 - SOFTEST) * note.velocity as f32 / 127.0)
}

/// How long after a note has ended its glow is still worth drawing, in
/// seconds.
pub const GLOW_TAIL: f64 = 5.0 * RELEASE_FALL;

/// How hard the picture is hit at `beat` by `hits` (sorted by time): each
/// hit strikes with its own strength and falls away over `fall` seconds;
/// of several still ringing, the strongest counts.
pub fn strike(hits: &[Hit], beat: Beats, time: &Time, fall: f64) -> f32 {
    // Nothing older than seven falls is still to be seen.
    let oldest = time.before(beat, 7.0 * fall);
    let first = hits.partition_point(|hit| hit.beat < oldest);
    let mut level = 0.0f32;
    for hit in hits[first..].iter().take_while(|hit| hit.beat <= beat) {
        let since = time.between(hit.beat, beat);
        level = level.max(hit.hard * (-since / fall).exp() as f32);
    }
    level
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPO: f64 = 120.0;

    fn steady() -> Time {
        Time::constant(TEMPO)
    }

    fn note(velocity: u8) -> Note {
        Note { pitch: 60, start: 4.0, length: 2.0, velocity }
    }

    #[test]
    fn a_note_is_dark_until_it_starts_and_brightest_when_it_does() {
        let note = note(127);
        assert_eq!(glow(&note, 3.999, &steady()), 0.0);
        assert_eq!(glow(&note, 4.0, &steady()), 1.0, "on the beat it starts on, not a frame later");
        let levels: Vec<f32> = [4.0, 4.1, 4.5, 5.9].iter().map(|beat| glow(&note, *beat, &steady())).collect();
        assert!(levels.windows(2).all(|pair| pair[1] < pair[0]), "{levels:?}");
        assert!((levels[3] - HELD).abs() < 0.02, "held, it settles: {levels:?}");
    }

    #[test]
    fn a_note_goes_out_soon_after_it_ends() {
        let note = note(127);
        let at_end = glow(&note, 6.0, &steady());
        let soon = glow(&note, 6.0 + 0.1 * TEMPO / 60.0, &steady());
        assert!(soon < at_end * 0.4, "{at_end} then {soon}");
        assert!(glow(&note, 6.0 + GLOW_TAIL * TEMPO / 60.0, &steady()) < 0.01);
    }

    #[test]
    fn a_soft_note_glows_less_than_a_hard_one() {
        let (soft, hard) = (glow(&note(20), 4.0, &steady()), glow(&note(127), 4.0, &steady()));
        assert!(soft > 0.4 && soft < 0.6 && hard == 1.0, "{soft} {hard}");
    }

    #[test]
    fn a_hit_strikes_on_its_beat_and_falls_away() {
        let hits = [Hit { beat: 8.0, hard: 0.8 }, Hit { beat: 9.0, hard: 0.5 }];
        assert_eq!(strike(&hits, 7.999, &steady(), KICK_FALL), 0.0, "not before it");
        assert_eq!(strike(&hits, 8.0, &steady(), KICK_FALL), 0.8, "on it");
        // A sixtieth of a second on, most of it is still there.
        let next_frame = strike(&hits, 8.0 + TEMPO / 60.0 / 60.0, &steady(), KICK_FALL);
        assert!(next_frame > 0.6 && next_frame < 0.8, "{next_frame}");
        // Half a second on it is gone, and the next hit strikes afresh.
        assert!(strike(&hits, 8.99, &steady(), KICK_FALL) < 0.01);
        assert_eq!(strike(&hits, 9.0, &steady(), KICK_FALL), 0.5);
        assert_eq!(strike(&[], 9.0, &steady(), KICK_FALL), 0.0);
    }

    #[test]
    fn what_falls_away_falls_in_seconds_whatever_the_tempo_does_under_it() {
        use trill_core::Project;
        // 120, and from beat 8 on twice as fast.
        let mut project = Project::new("faster");
        project.set_tempo(120.0).unwrap();
        project.set_tempo_at(8.0, 240.0, false).unwrap();
        let time = Time::of(&project);
        // A hit a quarter of a second old is as far gone on either side of the
        // change: half a beat before it, a whole beat after.
        let hits = [Hit { beat: 4.0, hard: 1.0 }, Hit { beat: 8.0, hard: 1.0 }];
        let (slow, fast) = (strike(&hits[..1], 4.5, &time, CRASH_FALL), strike(&hits[1..], 9.0, &time, CRASH_FALL));
        assert!(
            (slow - fast).abs() < 1e-6 && (slow - (-0.25f64 / CRASH_FALL).exp() as f32).abs() < 1e-6,
            "{slow} {fast}"
        );
        // And one struck before the change is timed across it: beat 7.5 to 8.5
        // is a quarter of a second and an eighth.
        let across = strike(&[Hit { beat: 7.5, hard: 1.0 }], 8.5, &time, CRASH_FALL);
        assert!((across - (-0.375f64 / CRASH_FALL).exp() as f32).abs() < 1e-6, "{across}");
        // A note held across the change is held for the seconds it lasts.
        let held = Note { pitch: 60, start: 7.0, length: 2.0, velocity: 127 };
        let (at_end, after) = (glow(&held, 9.0, &time), glow(&held, 9.0 + 0.1 * 4.0, &time));
        assert!((at_end - (HELD + (1.0 - HELD) * (-0.75f64 / FLASH_FALL).exp() as f32)).abs() < 1e-6, "{at_end}");
        assert!(after < at_end * 0.4, "a tenth of a second after it ended: {after}");
        assert_eq!(glow(&held, 6.99, &time), 0.0);
    }

    #[test]
    fn of_two_hits_still_ringing_the_stronger_counts() {
        let hits = [Hit { beat: 8.0, hard: 1.0 }, Hit { beat: 8.25, hard: 0.2 }];
        // An eighth of a second after a crash, a soft one does not dim it.
        let level = strike(&hits, 8.25, &steady(), CRASH_FALL);
        assert!((level - (-0.125f64 / CRASH_FALL).exp() as f32).abs() < 1e-6, "{level}");
    }
}
