//! Private supervisor-to-runtime execution coordination transport.
//!
//! The public simulation protocol deliberately does not expose these keys.
//! They are rooted below the execution-owned bus and carry only the
//! framework-owned execution messages.  Both sides use the same key builder so
//! a runtime cannot accidentally subscribe to a sibling instance's boundary.

use prost::Message;

use crate::bus::BusHandle;

/// The generated execution messages used by participants and the supervisor.
pub(crate) mod wire {
    include!(concat!(env!("OUT_DIR"), "/phoxal.execution.v1.rs"));
}

/// Relative root for the private execution coordination leg.
pub(crate) const EXECUTION_ROOT: &str = "execution/v1";

/// The exact Protobuf encoding used on the private execution leg.
pub(crate) const PROTOBUF_ENCODING: &str = "application/protobuf";

/// Compose an execution-control key for one runtime instance and one leg.
pub(crate) fn key(bus: &BusHandle, instance: &str, leg: &str) -> String {
    bus.full_key(&format!("{EXECUTION_ROOT}/{instance}/{leg}"))
}

/// Encode a generated execution message for direct Zenoh publication.
pub(crate) fn encode<M: Message>(message: &M) -> crate::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(message.encoded_len());
    message.encode(&mut bytes)?;
    Ok(bytes)
}

/// Decode a generated execution message from a Zenoh payload.
pub(crate) fn decode<M: Message + Default>(payload: &[u8]) -> crate::Result<M> {
    Ok(M::decode(payload)?)
}

/// Return whether a Zenoh sample has the exact private execution encoding.
pub(crate) fn has_encoding(sample: &zenoh::sample::Sample) -> bool {
    sample.encoding().to_string() == PROTOBUF_ENCODING
}
