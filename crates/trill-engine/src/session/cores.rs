//! The processors a song that plays is made on, and what it costs them.
//!
//! One thread cannot always make a block in the time it lasts. The engine
//! spreads the tracks of a block that do not feed one another over a few
//! threads ([`crate::team`]), once the song has shown that it needs them
//! and never before: a song that one thread plays with most of its time
//! to spare is left on that thread. How many it may have is asked and
//! said here, with what the audio thread has made of it of late: how much
//! of its time went, how many blocks came late, and what each track took.

use super::Session;
use crate::engine::{Load, usual_play_threads};
use crate::graph::Cost;
use trill_core::TrackId;

/// What making one track's blocks took: see [`Session::take_costs`].
#[derive(Clone, Debug, PartialEq)]
pub struct TrackCost {
    pub track: TrackId,
    pub name: String,
    pub cost: Cost,
}

/// What making the song's blocks took, part by part, since that was last
/// asked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Costs {
    /// Every track of the song, in its order. A track's cost is its
    /// devices' and its own: its notes, its audio, its fader.
    pub tracks: Vec<TrackCost>,
    /// The master's devices and its fader.
    pub master: Cost,
}

impl Session {
    /// How many threads the song that plays may be made on, the audio
    /// callback's own among them.
    pub fn play_threads(&self) -> usize {
        self.engine.play_threads()
    }

    /// Says how many threads the song that plays may be made on, the audio
    /// callback's own among them; `None` for what suits the machine
    /// ([`usual_play_threads`]). One has the callback make everything.
    /// Not an edit: the song is as it was, and sounds as it did.
    pub fn set_play_threads(&mut self, threads: Option<usize>) {
        let threads = threads.unwrap_or_else(usual_play_threads).max(1);
        if threads == self.engine.play_threads() {
            return;
        }
        self.engine.set_play_threads(threads);
        // The graph says what can be made at once, or says nothing.
        self.compile_and_swap();
    }

    /// Sets the share of a block's own time that making its tracks must
    /// take before the song that plays is spread over its threads. Zero
    /// spreads every block: for hearing, and measuring, that it makes no
    /// difference to what is played.
    pub fn spread_from(&mut self, share: f64) {
        self.engine.spread_from(share);
    }

    /// How the audio thread has kept up since this was last asked: how
    /// much of the time it had it used, how many buffers came late, and
    /// how many threads it is made on.
    pub fn take_load(&mut self) -> Load {
        self.engine.take_load()
    }

    /// What making each track's blocks took since this was last asked,
    /// and the master's. A share of one processor's time comes of a cost
    /// and the rate the engine runs at ([`Cost::share`]).
    pub fn take_costs(&mut self) -> Costs {
        let tracks = (self.project.tracks.iter())
            .filter_map(|track| {
                let meter = self.meters.tracks.get(&track.id)?;
                Some(TrackCost { track: track.id, name: track.name.clone(), cost: meter.take_cost() })
            })
            .collect();
        Costs { tracks, master: self.meters.master.take_cost() }
    }
}
