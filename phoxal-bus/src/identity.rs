//! Execution, producer, and timeline identities are owned by the runtime ABI.
//!
//! Two of them are Zenoh session identities, and this module is where that
//! equivalence is made concrete: an execution pins the run's router session,
//! and a producer is read back from the session that publishes. Both
//! conversions cross the value, never the storage bytes - `uhlc::ID` keeps its
//! bytes little-endian, so hexing the array would produce a byte-reversed
//! string that no longer matches Zenoh's own rendering of the same identity.
//!
//! These are free functions because `ZenohId` is foreign to this crate and the
//! Phoxal identities are foreign to Zenoh's, so neither side can carry the
//! conversion as an inherent method or a `From` impl.

use zenoh::config::ZenohId;

pub use phoxal_runtime_contract::{ExecutionId, InvalidIdentity, ProducerId, TimelineId};

use crate::error::{BusError, Result};

/// The Zenoh session identity a run's router opens with.
#[cfg(any(test, feature = "router"))]
pub(crate) fn zenoh_id_for(execution: ExecutionId) -> Result<ZenohId> {
    ZenohId::try_from(&u128::from(execution).to_le_bytes()[..]).map_err(|error| {
        BusError::Transport(format!(
            "execution {execution} is not a session id: {error}"
        ))
    })
}

/// The producer identity of a session, read back from the id Zenoh assigned it.
pub(crate) fn producer_from_zid(zid: ZenohId) -> Result<ProducerId> {
    ProducerId::try_from(u128::from_le_bytes(zid.to_le_bytes())).map_err(|error| {
        BusError::Transport(format!("session id '{zid}' is not a producer: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_execution_and_its_session_identity_render_identically() {
        let execution = ExecutionId::mint();
        let zid = zenoh_id_for(execution).expect("an execution is always a session id");
        assert_eq!(zid.to_string(), execution.to_string());

        // And back: the session that opened with it reports a producer whose
        // text is the same string again.
        let producer = producer_from_zid(zid).expect("a session id is always a producer");
        assert_eq!(producer.to_string(), execution.to_string());
    }

    #[test]
    fn a_narrow_session_identity_survives_the_round_trip_unpadded() {
        // Zenoh mints session ids uniformly across the value range, so roughly
        // one in sixteen renders narrower than an execution's pinned width. A
        // producer must carry that exactly, not pad it into a different string.
        let zid: ZenohId = "abc".parse().expect("a short session id is legal");
        let producer = producer_from_zid(zid).expect("a session id is always a producer");
        assert_eq!(producer.to_string(), "abc");
        assert_eq!(ProducerId::parse(&zid.to_string()), Ok(producer));
    }
}
