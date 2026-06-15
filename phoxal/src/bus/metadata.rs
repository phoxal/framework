use crate::bus::{Error, Result};

const NO_TIMESTAMP_LEN: usize = 1;
const TIMESTAMP_LEN: usize = 9;
const NO_TIMESTAMP: u8 = 0;
const HAS_TIMESTAMP: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusMetadata {
    pub produced_at_ns: Option<u64>,
}

impl BusMetadata {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(if self.produced_at_ns.is_some() {
            TIMESTAMP_LEN
        } else {
            NO_TIMESTAMP_LEN
        });
        match self.produced_at_ns {
            Some(produced_at_ns) => {
                bytes.push(HAS_TIMESTAMP);
                bytes.extend_from_slice(&produced_at_ns.to_le_bytes());
            }
            None => bytes.push(NO_TIMESTAMP),
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != NO_TIMESTAMP_LEN && bytes.len() != TIMESTAMP_LEN {
            return Err(Error::TypedDecode(format!(
                "invalid BusMetadata length: expected {NO_TIMESTAMP_LEN} bytes without timestamp or {TIMESTAMP_LEN} bytes with timestamp, received {}",
                bytes.len()
            )));
        }

        match (bytes[0], bytes.len()) {
            (NO_TIMESTAMP, NO_TIMESTAMP_LEN) => Ok(Self {
                produced_at_ns: None,
            }),
            (HAS_TIMESTAMP, TIMESTAMP_LEN) => Ok(Self {
                produced_at_ns: Some(u64::from_le_bytes([
                    bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                    bytes[8],
                ])),
            }),
            (NO_TIMESTAMP, TIMESTAMP_LEN) => Err(Error::TypedDecode(
                "invalid BusMetadata timestamp flag: flag says no timestamp but buffer carries timestamp bytes".to_string(),
            )),
            (HAS_TIMESTAMP, NO_TIMESTAMP_LEN) => Err(Error::TypedDecode(
                "invalid BusMetadata timestamp flag: flag says timestamp is present but buffer is too short".to_string(),
            )),
            (flag, _) => Err(Error::TypedDecode(format!(
                "invalid BusMetadata timestamp flag: expected {NO_TIMESTAMP} or {HAS_TIMESTAMP}, received {flag}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BusMetadata;

    #[test]
    fn metadata_roundtrips_without_timestamp() -> crate::bus::Result<()> {
        let metadata = BusMetadata {
            produced_at_ns: None,
        };
        let encoded = metadata.encode();

        assert_eq!(encoded.len(), 1);
        assert_eq!(BusMetadata::decode(&encoded)?, metadata);
        Ok(())
    }

    #[test]
    fn metadata_roundtrips_with_timestamp() -> crate::bus::Result<()> {
        let metadata = BusMetadata {
            produced_at_ns: Some(123_456_789),
        };
        let encoded = metadata.encode();

        assert_eq!(encoded.len(), 9);
        assert_eq!(BusMetadata::decode(&encoded)?, metadata);
        Ok(())
    }

    #[test]
    fn metadata_rejects_corrupt_buffer() {
        assert!(BusMetadata::decode(&[1, 2, 3]).is_err());
        assert!(BusMetadata::decode(&[2]).is_err());
        assert!(BusMetadata::decode(&[0, 9, 9, 9, 9, 9, 9, 9, 9]).is_err());
    }
}
