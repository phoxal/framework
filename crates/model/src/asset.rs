//! Canonical logical asset identities.
//!
//! The filesystem layout, integrity index, and participant access fence belong
//! to `phoxal-bundle`. The canonical model carries only the logical identity
//! that source compilation and the runtime document share.

use serde::{Deserialize, Serialize};

use crate::error::ModelError;

/// A normalized, forward-slash logical asset identifier.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AssetId(String);

impl AssetId {
    /// Validate and construct a logical asset identifier.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::MalformedAssetId`] unless `value` is a relative,
    /// forward-slash path with no empty, `.` or `..` segment. That is what
    /// makes an id unable to name anything outside a bundle asset root.
    pub fn new(value: impl Into<String>) -> Result<Self, ModelError> {
        let value = value.into();
        if value.is_empty()
            || value.starts_with('/')
            || value.contains('\\')
            || value
                .split('/')
                .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        {
            return Err(ModelError::MalformedAssetId { value });
        }
        Ok(Self(value))
    }

    /// The normalized forward-slash representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for AssetId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AssetId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_normalized_ids() {
        for invalid in ["", "/a", "../a", "a/../b", "a\\b", "a//b"] {
            assert!(AssetId::new(invalid).is_err(), "{invalid}");
        }
        assert_eq!(
            AssetId::new("meshes/base.stl").unwrap().as_str(),
            "meshes/base.stl"
        );
    }
}
