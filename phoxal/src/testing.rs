//! Explicit in-process participant testing support.
//!
//! This module is available only with the `test-harness` feature. It is not a
//! process launch protocol: tests supply a caller-owned bus and explicitly
//! drive the participant's lifecycle. Production participants enter through
//! [`crate::run`], which parses the supervised launch and opens its unique bus
//! owner.

use std::time::Duration;

use phoxal_runtime_contract::identity::{ParticipantId, ParticipantIdError};

pub use crate::participant::clock::ClockSource;
pub use crate::participant::clock::test::TestClock;
pub use crate::participant::runner::{run_test_harness, run_test_harness_with_clock};
pub use phoxal_runtime_contract::origin::ExecutionOrigin;

/// Explicit in-process test input.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct TestHarness {
    pub(crate) participant_id: ParticipantId,
    pub(crate) execution_origin: ExecutionOrigin,
    pub(crate) shutdown_grace: Duration,
    pub(crate) query_reply_delay: Option<Duration>,
}

impl TestHarness {
    /// Construct an explicit test-harness input for a participant id.
    pub fn new(participant_id: impl Into<String>) -> std::result::Result<Self, ParticipantIdError> {
        Ok(Self {
            participant_id: ParticipantId::new(participant_id)?,
            execution_origin: ExecutionOrigin::mint(),
            shutdown_grace: Duration::from_millis(
                crate::participant::launch::DEFAULT_SHUTDOWN_GRACE_MS,
            ),
            query_reply_delay: None,
        })
    }

    /// Use a caller-selected execution origin in a test clock domain.
    #[must_use]
    pub fn with_execution_origin(mut self, origin: ExecutionOrigin) -> Self {
        self.execution_origin = origin;
        self
    }

    /// Delay every test-harness query reply to exercise the runner's
    /// out-of-band reply transport. This is deliberately unavailable to a
    /// supervised participant process.
    #[doc(hidden)]
    #[must_use]
    pub fn with_query_reply_delay(mut self, delay: Duration) -> Self {
        self.query_reply_delay = Some(delay);
        self
    }
}
