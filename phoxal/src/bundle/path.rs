//! Canonical bundle-relative paths.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::__compat::wire::{DescribeWire, WireSchema};
use serde::{Deserialize, Serialize};

/// A normalized bundle-relative path: forward slashes only, no leading slash,
/// no empty, `.`, or `..` component.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BundlePath(String);

impl BundlePath {
    /// Validate a forward-slash relative path.
    pub fn new(value: impl Into<String>) -> Result<Self, BundlePathError> {
        let value = value.into();
        if value.is_empty() {
            return Err(BundlePathError::Empty);
        }
        if value.starts_with('/') {
            return Err(BundlePathError::Absolute(value));
        }
        if value.contains('\\') {
            return Err(BundlePathError::NotNormalized(value));
        }
        if value
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
        {
            return Err(BundlePathError::NotNormalized(value));
        }
        Ok(Self(value))
    }

    /// The normalized path string stored in JSON.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn filesystem_path(&self, root: &Path) -> PathBuf {
        root.join(self.0.split('/').collect::<PathBuf>())
    }
}

impl fmt::Display for BundlePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for BundlePath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for BundlePath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl DescribeWire for BundlePath {
    // Invariant: this states what the `Serialize` above writes - the normalized
    // forward-slash path as one string.
    fn wire_schema() -> WireSchema {
        WireSchema::opaque("BundlePath", WireSchema::String)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `BundlePath` has a hand-written serializer whose output shape the Rust
    /// declaration does not predict, so the declared shape is checked against a
    /// real serialized value.
    #[test]
    fn the_declared_shape_is_the_shape_its_serializer_writes() {
        let path = BundlePath::new("bin/brain").expect("a canonical bundle path");
        let json = serde_json::to_value(&path).expect("a bundle path serializes");
        assert_eq!(BundlePath::wire_schema().conforms(&json), Ok(()));
        assert_eq!(
            BundlePath::wire_schema(),
            WireSchema::opaque("BundlePath", WireSchema::String)
        );
    }

    /// Every way of naming something outside the bundle is refused, which is
    /// what makes an asset read unable to escape `assets/`.
    #[test]
    fn a_path_that_could_leave_the_bundle_is_refused() {
        for rejected in [
            "",
            "/etc/passwd",
            "assets/../../etc",
            "assets/./x",
            "assets\\x",
        ] {
            assert!(BundlePath::new(rejected).is_err(), "{rejected}");
        }
        assert_eq!(
            BundlePath::new("assets/robot/meshes/base.stl")
                .expect("a normalized relative path")
                .as_str(),
            "assets/robot/meshes/base.stl"
        );
    }
}

/// Why a bundle-relative path was rejected.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BundlePathError {
    #[error("bundle path is empty")]
    Empty,
    #[error("bundle path is absolute: '{0}'")]
    Absolute(String),
    #[error("bundle path is not normalized: '{0}'")]
    NotNormalized(String),
}
