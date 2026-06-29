//! The Zenoh-native `bus_abi` boundary (D26/D62).
//!
//! Samples are Zenoh-native: the key (`<namespace>/robots/<robot-id>/<topic>`),
//! an encoding string + a [`BusMetadata`] attachment (the version identity +
//! provenance), and a plain MessagePack body payload - there is no Phoxal frame
//! independent of Zenoh, and no `{"v":…}` version tag in the body (D62).

mod abi;
mod codec;
mod error;
mod handle;
mod metadata;
mod query;
mod server;
mod session;
mod topic;

pub use abi::{BUS_ABI, BusAbi, CodecId, encoding_string};
pub use codec::{Codec, CodecError, MessagePack};
pub use error::{BusError, Result};
pub use handle::{DEFAULT_QUERY_TIMEOUT, Latest, Publisher, Querier, Received, Subscriber};
pub use metadata::{BusMetadata, Source};
pub use query::{QueryCode, QueryError, QueryFailure, ServerResult};
pub use server::{IncomingQuery, ServerQueryable};
pub use session::{Bus, BusConfig, BusHealth};
pub use topic::{PubSub, Query, Topic, TopicKind, WildcardPublish};

/// Logical robot time: an epoch + a nanosecond timestamp in the clock's domain.
///
/// Every bus sample's `produced_at_ns`/`epoch` is stamped from a `LogicalTime`,
/// so all participants share one time domain (D34). Within an epoch `time_ns`
/// strictly increases; an epoch bump signals a reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LogicalTime {
    epoch: u64,
    time_ns: u64,
}

impl LogicalTime {
    /// Construct a logical time from an epoch + nanosecond timestamp.
    pub const fn new(epoch: u64, time_ns: u64) -> Self {
        LogicalTime { epoch, time_ns }
    }

    /// The clock epoch (reset counter).
    pub const fn epoch(self) -> u64 {
        self.epoch
    }

    /// The nanosecond timestamp within the epoch.
    pub const fn time_ns(self) -> u64 {
        self.time_ns
    }
}

#[cfg(test)]
mod tests;
