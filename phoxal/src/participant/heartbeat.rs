//! Framework-owned participant presence heartbeats.
//!
//! Heartbeats are runner infrastructure, like bus logs: every checked participant
//! gets them from the runner, but they are not part of the participant-authored
//! `emit-apis` contract surface.

use std::time::Duration;

use phoxal_api::v1 as api;
use phoxal_bus::{Bus, LogicalTime, Publisher};

/// Runner heartbeat cadence.
///
/// One second is comfortably below the presence service's 3 s stale threshold,
/// leaves room for one missed tick before degradation, and is still low-volume
/// enough to be framework background traffic rather than participant work.
pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) struct HeartbeatPublisher {
    participant_id: String,
    publisher: Option<Publisher<api::presence::Heartbeat>>,
    readiness: api::presence::Readiness,
}

impl HeartbeatPublisher {
    pub(crate) fn attach(bus: Bus, participant_id: impl Into<String>) -> Self {
        let publisher = Publisher::new(bus, &api::topic::new().presence().heartbeat())
            .map_err(|error| {
                tracing::warn!(
                    target: "phoxal.runtime",
                    error = %error,
                    "presence heartbeat publisher could not be created"
                );
                error
            })
            .ok();

        Self {
            participant_id: participant_id.into(),
            publisher,
            readiness: api::presence::Readiness::Initializing,
        }
    }

    #[cfg(test)]
    fn disabled(participant_id: impl Into<String>) -> Self {
        Self {
            participant_id: participant_id.into(),
            publisher: None,
            readiness: api::presence::Readiness::Initializing,
        }
    }

    pub(crate) fn set_readiness(&mut self, readiness: api::presence::Readiness) {
        self.readiness = readiness;
    }

    pub(crate) fn publish(&self, at: LogicalTime) {
        let Some(publisher) = &self.publisher else {
            return;
        };

        let heartbeat = api::presence::Heartbeat {
            participant: self.participant_id.clone(),
            readiness: self.readiness.clone(),
        };
        if let Err(error) = publisher.try_publish(at, heartbeat) {
            tracing::warn!(
                target: "phoxal.runtime",
                error = %error,
                participant = %self.participant_id,
                "presence heartbeat publish failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_heartbeat_publisher_is_a_noop() {
        let mut heartbeat = HeartbeatPublisher::disabled("offline");
        heartbeat.set_readiness(api::presence::Readiness::Ready);

        heartbeat.publish(LogicalTime::new(0, 1));
    }
}
