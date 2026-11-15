//! Stable identifiers. Ids are allocated by the [`Project`](crate::Project)
//! and are never reused within a project, so they stay valid across undo/redo.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id_type {
    ($name:ident, $label:literal) => {
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($label, " {}"), self.0)
            }
        }
    };
}

id_type!(TrackId, "track");
id_type!(ClipId, "clip");
id_type!(DeviceId, "device");
