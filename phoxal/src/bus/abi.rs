//! `bus_abi` — the framework-owned wire envelope (D26/D45/D62).
//!
//! `bus_abi` is the **one globally-coordinated layer**: the Zenoh key scheme, the
//! encoding-string format, the attachment/[`BusMetadata`](super::BusMetadata)
//! layout, and the codec id. It is a **constant**, not a manifest field, and is
//! **orthogonal** to `api_version` (D62) — the contract bodies are
//! api-version-local types; only the envelope around them is `bus_abi`.
//!
//! The cross-artifact `bus_abi` compatibility check is deferred to freeze (D45):
//! within a coherent platform release every participant is rebuilt from one
//! framework version, so the constant simply has to be defined and freezable. The
//! runtime decode path (encoding/metadata fast-reject) is the backstop.

use std::fmt;

/// The framework-owned wire-envelope identifier.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BusAbi {
    id: &'static str,
}

impl BusAbi {
    /// The `bus_abi` identifier string (e.g. `"phoxal-bus/v0"`).
    pub const fn id(&self) -> &'static str {
        self.id
    }
}

impl fmt::Debug for BusAbi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BusAbi({})", self.id)
    }
}

impl fmt::Display for BusAbi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id)
    }
}

/// The current, pre-stable (`v0`) bus ABI. Freezes to `v1` per-axis (D27),
/// independently of any API version.
pub const BUS_ABI: BusAbi = BusAbi {
    id: "phoxal-bus/v0",
};

/// The wire codec identifier carried in bus metadata. One codec in v1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CodecId {
    /// MessagePack with named fields (`rmp_serde`).
    MessagePack = 1,
}

impl CodecId {
    /// The numeric id carried on the wire.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Resolve a wire codec id, rejecting anything unknown before body decode.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(CodecId::MessagePack),
            _ => None,
        }
    }
}

/// Build the Zenoh encoding string that mirrors the contract metadata so a
/// receiver can fast-reject before decoding the body.
///
/// Format: `phoxal/v0;family=<family>;api=<api_version>;codec=<id>`.
pub fn encoding_string(family: &str, api_version: &str, codec: CodecId) -> String {
    format!(
        "phoxal/v0;family={family};api={api_version};codec={}",
        codec.as_u8()
    )
}
