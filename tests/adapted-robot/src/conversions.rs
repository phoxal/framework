//! The robot author's own conversion code, in a normal source module.
//!
//! There is no mapping language, adapter selector, or registration: the
//! generated types are local to this crate, so an ordinary `From`
//! implementation composes, and Rust coherence keeps it unique.

use crate::api::brain::BrainWorldStatus;
use crate::api::navigation::MapState;
use crate::api::world::WorldRevision;

impl From<WorldRevision> for MapState {
    fn from(source: WorldRevision) -> Self {
        Self {
            revision: source.revision,
            available: source.available,
            // The producer's capture time is preserved; the conversion
            // never stamps its own time over the observation's source.
            oldest_capture_time_nanos: source.oldest_capture_time_nanos,
        }
    }
}

impl From<WorldRevision> for BrainWorldStatus {
    fn from(source: WorldRevision) -> Self {
        Self {
            revision: source.revision,
            available: source.available,
        }
    }
}
