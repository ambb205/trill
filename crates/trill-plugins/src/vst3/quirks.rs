//! What is known of particular plugins that a host has to work around.
//!
//! A plugin that breaks the rules of its format in a way that takes the
//! process down is the plugin's fault and the host's problem: the user
//! has it installed, and the program must not crash. What can be done
//! from outside is written down here, each with what was seen, so that
//! it can be measured again and taken out when the plugin is mended.
//!
//! # Neural DSP's Archetypes: taken down only when they have been heard out
//!
//! An Archetype goes on setting itself up, on threads of its own, for
//! some seconds after it has been made or handed a state. When that work
//! is done it says so to the plugin's user-interface thread, in a message
//! the plugin posts to a window of its own. The message refers to the
//! instance, and is delivered whether or not the instance is still there.
//! If the instance has been destroyed meanwhile and another instance of
//! the plugin is alive (so that the plugin's message window is), the
//! message is handled on a destroyed object: an access violation inside
//! the plugin, on the plugin main thread, entering a critical section
//! that has been deleted.
//!
//! A host that keeps a plugin for as long as its track exists never sees
//! this. This one makes an instance for every probe, measurement and
//! render and lets it go seconds later, beside the one that plays the
//! song.
//!
//! So two things have to be so when an Archetype is taken down: its work
//! is done, and what that work posted has been delivered. The host cannot
//! see the first and can only give it time; the second is the host's own
//! doing, because the messages are delivered by the plugin thread, between
//! its jobs.
//!
//! Measured with `tests/lifetimes.rs` (Archetype Cory Wong X 1.x, one
//! instance kept, others made from its state, played and destroyed; the
//! machine busy, all its processors in use): destroyed at once, the
//! process died in 4 attempts of 4, by the third instance; destroyed 3 s
//! after being made, in 3 of 4, by the eleventh; after 10 s, in none of 4
//! (48 instances). On a machine with time to spare the work is done
//! sooner: destroyed at once, none of 8 attempts died at 39% load. How
//! the instance is taken down made no difference that could be told from
//! chance: delivering the thread's pending messages first, before or
//! after deactivating, waiting between deactivating and destroying, or
//! telling it to stop processing on the main thread.
//!
//! An Archetype that is let go of is therefore kept, as it is, and taken
//! down by the plugin thread itself (`Vst3Thread::served`):
//!
//! * when it is [`ARCHETYPE_PATIENCE`] old, counted in the time the
//!   thread was free for its windows. The time jobs kept the thread is
//!   time in which the plugin could not be told anything, and counts
//!   towards no instance's age;
//! * and only at the one place in the thread's loop where the messages
//!   that were waiting have just been delivered, one instance to a turn.
//!
//! The second was learned from what the first alone let through. Kept
//! for fifteen seconds by the clock and taken down in the thread's
//! idling, the amplifier still ended 4 of 19 attempts at six renders of a
//! song with three of them, all processors in use. The three of those
//! that were traced died the same way: the thread had just made an
//! instance for the next render, which had kept it from its windows for 4
//! to 23 seconds; the kept instance's time ran out meanwhile, it was
//! taken down on the way out of that job, and the messages that had
//! waited were delivered a moment later, to an instance that was gone.
//! The scripted plugin does the same (`kind::SETS_UP_BEHIND`), so the
//! default tests hold this.
//!
//! What this costs is the memory of an amplifier for some seconds more.
//! It does not make the plugin correct: its work could take longer than
//! the host waits, on a machine slow enough, and the crash would be back.
//! The only thing that contains every fault of this kind is a process of
//! the plugin's own.

use super::class_id::ClassId;
use std::time::Duration;

/// How old an Archetype has to be before it is taken down: half as long
/// again as what was measured to be enough.
const ARCHETYPE_PATIENCE: Duration = Duration::from_secs(15);

/// How long after it was made, or last handed a state, an instance of
/// the plugin `class` has to be left alone before it may be taken down,
/// in time the plugin thread was free for its windows. Nothing for all but
/// the plugins this module knows to need it; an instance of one of those
/// is taken down by the plugin thread, when it has served its windows.
pub(super) fn patience(class: ClassId) -> Duration {
    #[cfg(test)]
    if super::scripted::sets_itself_up_behind(class) {
        return super::scripted::SETTING_UP * 3;
    }
    if crate::vendor::archetype::is_ours(&class.to_string()) { ARCHETYPE_PATIENCE } else { Duration::ZERO }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_plugins_that_are_known_to_need_it_are_waited_for() {
        let class = |id: &str| ClassId::parse(id).unwrap();
        // Any of the maker's, whatever the product.
        assert_eq!(patience(class("ABCDEF019182FAEB4E4453504E435758")), ARCHETYPE_PATIENCE);
        assert_eq!(patience(class("ABCDEF019182FAEB4E4453504E544858")), ARCHETYPE_PATIENCE);
        // Another maker's plugin built with the same framework, and a
        // plugin of no framework at all.
        assert_eq!(patience(class("ABCDEF019182FAEB4170736441677066")), Duration::ZERO);
        assert_eq!(patience(class("56534558667350736572756D20320000")), Duration::ZERO);
    }
}
