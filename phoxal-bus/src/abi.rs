//! `bus_abi` - the framework-owned wire envelope (D26/D45/D62).
//!
//! `bus_abi` is the **one globally-coordinated layer**, the identity of which is
//! the combination of three things wrapped around every body:
//!
//! 1. the Zenoh **encoding string** ([`encoding_string`]:
//!    `phoxal/v0;family=<family>;api=<api_version>;codec=<id>`), which lets a
//!    receiver fast-reject on family/api/codec before touching the payload;
//! 2. the Zenoh **attachment** - a MessagePack [`BusMetadata`](super::BusMetadata)
//!    carrying the same version identity plus provenance (source + logical time);
//! 3. the **codec** id ([`CodecId`]) naming how the body payload is encoded
//!    (MessagePack, the one codec in v1).
//!
//! Together with the key scheme (`<namespace>/robots/<robot-id>/<topic>`) these
//! make up the wire contract. `bus_abi` is a **constant** ([`BUS_ABI`]), not a
//! manifest field, and is **orthogonal** to `api_version` (D62) - the contract
//! bodies are api-version-local types; only the envelope around them is `bus_abi`.
//!
//! The cross-artifact `bus_abi` compatibility check is deferred to freeze (D45):
//! within a coherent platform release every participant is rebuilt from one
//! framework version, so the constant simply has to be defined and freezable. The
//! decode path at runtime (encoding/metadata fast-reject) is the backstop.

use std::fmt;

/// The framework-owned wire-envelope identifier (a versioned string such as
/// `"phoxal-bus/v0"`). One value, [`BUS_ABI`], is the live ABI; it freezes
/// per-axis to `v1` (D27), independently of any API version.
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

/// The parsed Zenoh encoding-string half of `bus_abi`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodingMetadata {
    /// Contract family id (`<Body as ContractBody>::FAMILY`).
    pub family: String,
    /// Graph API version (`<Body::Api as ApiVersion>::ID`).
    pub api_version: String,
    /// Numeric codec id.
    pub codec: u8,
}

impl EncodingMetadata {
    /// The codec id, if recognized by this `bus_abi`.
    pub fn codec_id(&self) -> Option<CodecId> {
        CodecId::from_u8(self.codec)
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

/// Parse and validate the Zenoh encoding string owned by `bus_abi`.
///
/// Format: `phoxal/v0;family=<family>;api=<api_version>;codec=<id>`.
pub fn parse_encoding_string(value: &str) -> std::result::Result<EncodingMetadata, String> {
    let mut parts = value.split(';');
    let prefix = parts
        .next()
        .ok_or_else(|| "encoding string is empty".to_string())?;
    if prefix != "phoxal/v0" {
        return Err(format!(
            "expected encoding prefix 'phoxal/v0', got '{prefix}'"
        ));
    }

    let mut family = None;
    let mut api_version = None;
    let mut codec = None;
    for field in parts {
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| format!("encoding field '{field}' is missing '='"))?;
        if value.is_empty() {
            return Err(format!("encoding field '{key}' is empty"));
        }
        match key {
            "family" => set_once(&mut family, key, value.to_string())?,
            "api" => set_once(&mut api_version, key, value.to_string())?,
            "codec" => {
                let parsed = value
                    .parse::<u8>()
                    .map_err(|_| format!("encoding field 'codec' is not a u8: '{value}'"))?;
                set_once(&mut codec, key, parsed)?;
            }
            _ => return Err(format!("unknown encoding field '{key}'")),
        }
    }

    Ok(EncodingMetadata {
        family: family.ok_or_else(|| "encoding string is missing family".to_string())?,
        api_version: api_version.ok_or_else(|| "encoding string is missing api".to_string())?,
        codec: codec.ok_or_else(|| "encoding string is missing codec".to_string())?,
    })
}

fn set_once<T>(slot: &mut Option<T>, key: &str, value: T) -> std::result::Result<(), String> {
    if slot.replace(value).is_some() {
        Err(format!("duplicate encoding field '{key}'"))
    } else {
        Ok(())
    }
}
