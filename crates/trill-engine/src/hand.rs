//! Whose hand is on the session.
//!
//! One thing a session does is not a matter of what the song says: it
//! opens a microphone, which hears a room. The song can be anybody's, and
//! so can the edits that reach a session: an assistant that reads the web
//! works at a door of its own, where there is nobody to ask. So the input
//! is opened for a track only by the user's own hand
//! ([`Session::set_armed`](crate::Session::set_armed) and what stands
//! beside it there), and a thread says that what it does is the user's
//! hand by taking a guard here.
//!
//! **Saying nothing is not being the user.** A thread that has taken no
//! guard is nobody's hand: what it does to a session is done, and opens no
//! input. A front end that forgets to say so has a switch that waits for a
//! click, not a microphone that is open. The window says so around what
//! its own thread asks of the session; the MCP server says so for a client
//! the user set up and never for the assistant's door; tests say so where
//! they stand for the user.

use std::cell::Cell;
use std::marker::PhantomData;

thread_local! {
    static BY_HAND: Cell<bool> = const { Cell::new(false) };
}

/// Says that what this thread asks of a session, until the guard that is
/// returned is dropped, is asked by the user's own hand. Guards nest; each
/// puts back what was there before it.
///
/// A future that works for the user across awaits takes a guard each time
/// it is polled and lets go of it before it yields: between polls the
/// thread is someone else's.
pub fn by_hand() -> ByHand {
    ByHand { before: BY_HAND.replace(true), _here: PhantomData }
}

/// While this lives, the thread it was made on is the user's hand; see
/// [`by_hand`].
#[must_use = "the thread is the user's hand only while the guard lives"]
pub struct ByHand {
    before: bool,
    /// Made and dropped on one thread.
    _here: PhantomData<*const ()>,
}

impl Drop for ByHand {
    fn drop(&mut self) {
        BY_HAND.set(self.before);
    }
}

/// Whether this thread said it is the user's hand.
pub(crate) fn is_by_hand() -> bool {
    BY_HAND.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_is_the_users_hand_only_while_it_says_so_and_no_other_thread_is() {
        assert!(!is_by_hand(), "saying nothing is not being the user");
        {
            let _outer = by_hand();
            assert!(is_by_hand());
            {
                let _inner = by_hand();
                assert!(is_by_hand());
            }
            assert!(is_by_hand(), "a guard puts back what was there before it");
            assert!(!std::thread::spawn(is_by_hand).join().unwrap(), "another thread is nobody's hand");
        }
        assert!(!is_by_hand());
    }
}
