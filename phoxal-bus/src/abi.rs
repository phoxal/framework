//! The Zenoh encoding-string half of the wire envelope (D62, D1 wire-key fold).
//!
//! A version-qualified Zenoh key carries wire identity
//! (`<Body as ContractBody>::TOPIC`, e.g. `v0.1/drive/target`), while the
//! encoding string records the codec. A receiver therefore sees only samples
//! for its subscribed contract key and validates their encoding.

/// The bus ABI a participant binary declares in its embedded compatibility
/// record: the key grammar, the sample metadata, and the encoding string
/// together.
///
/// Distinct from the `phoxal/v0` prefix inside [`encoding_string`]: that token
/// is per-sample wire overhead and stays short, while this one is a document
/// identifier that has to be unambiguous alongside the launch and manifest
/// schemas it sits next to.
pub const BUS_ABI: &str = "phoxal/bus/v0";

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
