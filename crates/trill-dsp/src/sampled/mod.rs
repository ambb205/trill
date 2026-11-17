//! The sampled instrument: plays an instrument that was recorded note by
//! note.

pub mod loaded;
pub mod model;
pub mod pool;
pub mod read;

pub use loaded::{Loaded, RegionBox, State};
pub use model::{Instrument, Region, Summary, Unsupported};
