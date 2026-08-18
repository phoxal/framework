//! Explicit in-process participant testing support.
//!
//! This module is available only with the `test-harness` feature. It is not a
//! process launch protocol: tests supply a caller-owned bus and explicitly
//! drive the participant's lifecycle. Production participants enter through
//! [`crate::run`], which parses the supervised launch and opens its unique bus
//! owner.

use std::time::Duration;

use crate::identity::{ParticipantId, ParticipantIdError};

pub use crate::identity::TimelineId;
pub use crate::participant::clock::ClockSource;
pub use crate::participant::clock::test::TestClock;
pub use crate::participant::runner::{run_test_harness, run_test_harness_with_clock};

/// Explicit in-process test input.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct TestHarness {
    pub(crate) participant_id: ParticipantId,
    pub(crate) timeline: TimelineId,
    pub(crate) shutdown_grace: Duration,
    pub(crate) query_reply_delay: Option<Duration>,
}

impl TestHarness {
    /// Construct an explicit test-harness input for a participant id.
    ///
    /// A launched participant derives its real timeline from the execution the
    /// router reports; a harness has no router, so it mints one here and lets a
    /// test name it explicitly when two harnesses have to share a world history.
    pub fn new(participant_id: impl Into<String>) -> std::result::Result<Self, ParticipantIdError> {
        Ok(Self {
            participant_id: ParticipantId::new(participant_id)?,
            timeline: TimelineId::mint(),
            shutdown_grace: crate::participant::launch::SHUTDOWN_GRACE,
            query_reply_delay: None,
        })
    }

    /// Use a caller-selected timeline for this harness's robot time.
    #[must_use]
    pub fn with_timeline(mut self, timeline: TimelineId) -> Self {
        self.timeline = timeline;
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
