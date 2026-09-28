//! Robot-owned conversion between World's output and Navigation's input.

use crate::api::navigation::MapState;
use crate::api::world::WorldRevision;

impl From<WorldRevision> for MapState {
    fn from(source: WorldRevision) -> Self {
        Self {
            revision: source.revision,
            available: source.available,
            oldest_capture_time_nanos: source.oldest_capture_time_nanos,
        }
    }
}
