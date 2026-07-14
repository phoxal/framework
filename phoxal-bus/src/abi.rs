//! The Zenoh encoding-string half of the wire envelope (D62, D1 wire-key fold).
//!
//! Identity used to live in a separately-maintained `bus_abi` (a `family` /
//! `api_version` / `schema_id` triple carried in the encoding string and mirrored
//! in [`BusMetadata`](crate::metadata::BusMetadata)). That axis is gone: the
//! version is now folded into the Zenoh key itself
//! (`<Body as ContractBody>::TOPIC` is version-qualified, e.g.
//! `v1/drive/target`), so different versioned contracts physically cannot
//! collide on one key, and a receiver only ever sees samples on keys it
//! subscribed to. There is nothing left to fast-reject on except the codec, so
//! the encoding string and [`BusMetadata`] both shrink to that.

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

/// The parsed Zenoh encoding string: just the codec now that identity lives in
/// the key (D1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodingMetadata {
    /// Numeric codec id.
    pub codec: u8,
}

impl EncodingMetadata {
    /// The codec id, if recognized by this wire ABI.
    pub fn codec_id(&self) -> Option<CodecId> {
        CodecId::from_u8(self.codec)
    }
}

/// Build the Zenoh encoding string. Format: `phoxal/v0;codec=<id>`.
pub fn encoding_string(codec: CodecId) -> String {
    format!("phoxal/v0;codec={}", codec.as_u8())
}

/// Parse and validate the Zenoh encoding string. Format: `phoxal/v0;codec=<id>`.
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

    let mut codec = None;
    for field in parts {
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| format!("encoding field '{field}' is missing '='"))?;
        if value.is_empty() {
            return Err(format!("encoding field '{key}' is empty"));
        }
        match key {
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
