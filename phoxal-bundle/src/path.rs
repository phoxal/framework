//! Canonical bundle-relative paths and digest values.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A normalized, traversal-proof bundle-relative path.
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

    pub(crate) fn starts_with_directory(&self, directory: &str) -> bool {
        self.0
            .strip_prefix(directory)
            .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
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

/// A SHA-256 digest rendered as exactly 64 lowercase hexadecimal characters.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Sha256Digest(pub(crate) [u8; 32]);

impl Sha256Digest {
    /// Hash one byte sequence.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    /// Stream one reader into the digest without buffering the complete file.
    pub fn from_reader(mut reader: impl Read) -> std::io::Result<Self> {
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(Self(hasher.finalize().into()))
    }

    /// Parse the canonical JSON representation.
    pub fn parse(value: &str) -> Result<Self, DigestError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(DigestError(value.to_string()));
        }
        let mut bytes = [0; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = (hex(pair[0])? << 4) | hex(pair[1])?;
        }
        Ok(Self(bytes))
    }

    /// Render the canonical lowercase hexadecimal representation.
    #[must_use]
    pub fn as_hex(self) -> String {
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            output.push(hex_digit(byte >> 4));
            output.push(hex_digit(byte & 0x0f));
        }
        output
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.as_hex())
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_hex())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

fn hex(value: u8) -> Result<u8, DigestError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(DigestError(String::from("non-hex digest"))),
    }
}

const fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'a' + value - 10) as char,
    }
}

/// A digest that was not the canonical lowercase SHA-256 spelling.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("digest must be 64 lowercase hexadecimal characters, got '{0}'")]
pub struct DigestError(String);

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
