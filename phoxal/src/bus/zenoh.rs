use serde::{Serialize, de::DeserializeOwned};
use zenoh::bytes::{Encoding, ZBytes};

pub(crate) fn typed_encoding(schema_name: &str) -> Encoding {
    Encoding::from(format!("zenoh-ext/typed:{schema_name}"))
}

pub(crate) fn serialize_payload<T: Serialize>(value: &T) -> crate::bus::Result<ZBytes> {
    rmp_serde::to_vec_named(value)
        .map(ZBytes::from)
        .map_err(crate::bus::Error::from)
}

pub(crate) fn deserialize_payload<T: DeserializeOwned>(
    payload: &ZBytes,
) -> Result<T, rmp_serde::decode::Error> {
    rmp_serde::from_slice(payload.to_bytes().as_ref())
}

pub trait BusyResponse {
    fn busy() -> Self;
}
