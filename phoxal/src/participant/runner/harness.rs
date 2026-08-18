//! The explicit in-process test harness: its input, and the caller-owned bus
//! it runs on.
//!
//! Everything here is reached through [`crate::testing`], which the
//! `test-harness` profile declares. It lives in the runner because that is what
//! it drives, and because opening a bus is `BusOwner`'s job and `BusOwner` is
//! crate-private: a test that could open one itself would be holding the raw
//! transport no consumer profile receives.

use std::time::Duration;

use crate::bus::session::BusConfig;
use crate::bus::{
    BusCloseReport, BusHandle, BusOwner, ExecutionId, RobotInstant, SourceLabel, StepToken,
};
use crate::identity::{ParticipantId, ParticipantIdError, TimelineId};

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
    ///
    /// # Errors
    ///
    /// Returns [`ParticipantIdError`] when `participant_id` is not a valid
    /// participant identity.
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

/// A caller-owned in-process bus, for a test that drives a participant or
/// builds handles directly.
///
/// It connects to no endpoint: the session is in-process, so a test exercises
/// the real transport - real keys, real codec, real admission - without a
/// router, and two tests running at once cannot see each other because each
/// mints its own execution.
///
/// This is the supported way to get a [`BusHandle`] outside the runner. The
/// session owner itself stays crate-private: a test needs a bus, not the
/// authority to own one.
pub struct TestBus {
    owner: Option<BusOwner>,
    handle: BusHandle,
    execution: ExecutionId,
}

impl TestBus {
    /// Open an in-process bus that publishes under one participant identity.
    ///
    /// This is what a participant's own tests want: samples carry the same
    /// participant attribution a launched process would produce, so lease
    /// admission and provenance behave as they do in a real execution.
    ///
    /// # Errors
    ///
    /// Returns an error when `participant` is not a valid participant identity,
    /// or when the session cannot be opened.
    pub async fn for_participant(participant: &str) -> crate::Result<Self> {
        let execution = ExecutionId::mint();
        Self::open(
            BusConfig::for_participant(execution, ParticipantId::new(participant)?, Vec::new()),
            execution,
        )
        .await
    }

    /// Open an in-process bus that publishes as an external client carrying a
    /// diagnostic `label`, the way an attached application or a simulator does.
    ///
    /// # Errors
    ///
    /// Returns an error when `label` is not a representable source label, or
    /// when the session cannot be opened.
    pub async fn external(label: &str) -> crate::Result<Self> {
        let execution = ExecutionId::mint();
        Self::open(
            BusConfig::for_external(execution, Some(SourceLabel::new(label)?), Vec::new()),
            execution,
        )
        .await
    }

    async fn open(config: BusConfig, execution: ExecutionId) -> crate::Result<Self> {
        let (owner, handle) = BusOwner::open(config).await?;
        Ok(Self {
            owner: Some(owner),
            handle,
            execution,
        })
    }

    /// The handle every typed operation is built from.
    #[must_use]
    pub fn handle(&self) -> &BusHandle {
        &self.handle
    }

    /// The execution this bus is rooted at.
    #[must_use]
    pub fn execution(&self) -> ExecutionId {
        self.execution
    }

    /// Close the session and report what it took with it.
    ///
    /// Dropping a `TestBus` closes it too, without waiting: a test that wants
    /// deterministic close evidence calls this.
    pub async fn close(mut self) -> BusCloseReport {
        match self.owner.take() {
            Some(owner) => owner.close().await,
            None => BusCloseReport::default(),
        }
    }
}

impl std::fmt::Debug for TestBus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TestBus")
            .field("execution", &self.execution)
            .finish_non_exhaustive()
    }
}

/// Mint the step a test publishes a state at.
///
/// The runner mints one per released `Participant::step`, and a state
/// publisher takes nothing else. A test that publishes *into* the participant
/// under test is standing in for a peer's runner, so it gets the same token -
/// under the profile that says out loud this is a test.
#[must_use]
pub fn step_token(at: RobotInstant) -> StepToken {
    StepToken::mint(at)
}
