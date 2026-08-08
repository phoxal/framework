//! Runtime document wire decoding.

use crate::{BundleError, RuntimeDocument};

/// Decode the one schema-tagged document retained in an installed bundle.
pub(crate) fn decode(bytes: &[u8]) -> Result<RuntimeDocument, BundleError> {
    serde_json::from_slice(bytes).map_err(BundleError::from)
}
