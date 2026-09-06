//! Typed Live simulator publication and setpoint reception.

use super::*;

/// A simulator sample publisher bound to the exact Active controller revision.
pub struct LiveSamplePublisher<E: RobotEndpoint + Endpoint<Semantics = Sample>> {
    pub(super) inner: SamplePublisher<E>,
    pub(super) bus: BusHandle,
}

impl<E> Clone for LiveSamplePublisher<E>
where
    E: RobotEndpoint + Endpoint<Semantics = Sample>,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            bus: self.bus.clone(),
        }
    }
}

impl<E> LiveSamplePublisher<E>
where
    E: RobotEndpoint + Endpoint<Semantics = Sample>,
{
    /// Publish one sample from `transition` only while its exact controller
    /// and supervisor revision remain Active.
    pub fn publish(&self, transition: &LiveTransitionStamp, body: E) -> Result<(), SimulatorError> {
        let admitted = self.inner.publish_active_simulation(
            self.bus.producer(),
            transition.revision,
            crate::bus::CaptureStamp::exact(transition.instant()),
            body,
        )?;
        ensure_live_publication(admitted)
    }
}

/// A simulator state publisher that can emit only under the exact current
/// Active controller binding.
pub struct LiveStatePublisher<E: RobotEndpoint + Endpoint<Semantics = State>> {
    pub(super) inner: StatePublisher<E>,
    pub(super) bus: BusHandle,
}

impl<E> Clone for LiveStatePublisher<E>
where
    E: RobotEndpoint + Endpoint<Semantics = State>,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            bus: self.bus.clone(),
        }
    }
}

impl<E> LiveStatePublisher<E>
where
    E: RobotEndpoint + Endpoint<Semantics = State>,
{
    /// Publish state from `transition` only while its exact controller and
    /// supervisor revision remain Active.
    pub fn publish(&self, transition: &LiveTransitionStamp, body: E) -> Result<(), SimulatorError> {
        let admitted = self.inner.publish_active_simulation(
            self.bus.producer(),
            transition.revision,
            transition,
            body,
        )?;
        ensure_live_publication(admitted)
    }
}
pub struct LiveSetpointReceiver<E: RobotEndpoint + Endpoint<Semantics = Setpoint>> {
    pub(super) inner: SetpointReceiver<E>,
    pub(super) attachment: tokio::sync::watch::Receiver<Option<SimulationAttachmentState>>,
}

impl<E> LiveSetpointReceiver<E>
where
    E: RobotEndpoint + Endpoint<Semantics = Setpoint>,
{
    /// Take the next buffered command that belongs to `transition`.
    /// Commands from Preparing, Removing, a prior Active revision, or without
    /// revision evidence are discarded and can never become live later.
    pub fn try_recv_for(&self, transition: &LiveTransitionStamp) -> Option<Observed<E>> {
        self.try_recv_revision(transition.world, transition.revision)
    }

    /// Take the next buffered command for a current pre-transition Active
    /// boundary without inventing world progress.
    pub fn try_recv_at(&self, boundary: &ActiveBoundaryStamp) -> Option<Observed<E>> {
        self.try_recv_revision(boundary.world, boundary.revision)
    }

    fn try_recv_revision(&self, world: WorldInstanceId, revision: u64) -> Option<Observed<E>> {
        let active = self.attachment.borrow().is_some_and(|state| {
            state.phase == SimulationAttachmentPhase::Active
                && state.world == world
                && state.revision == revision
        });
        if !active {
            self.flush();
            return None;
        }
        while let Some(observed) = self.inner.try_recv() {
            if observed.metadata.attachment_revision == Some(revision) {
                return Some(observed);
            }
        }
        None
    }

    /// Drain every currently buffered command for `transition` through the
    /// capability's fixed-source lease.
    ///
    /// The lease remains the owner of source liveness, monotonic silence and
    /// hold expiry, stale sequence rejection, and fail-closed selection. Feed
    /// it [`ParticipantReadyEvents`] from [`SimulatorSession::participant_ready_events`],
    /// call this immediately before a native transition, then select with
    /// [`FixedSourceLease::live_host`] at the transition's host-monotonic
    /// boundary.
    pub fn drain_into(
        &self,
        transition: &LiveTransitionStamp,
        lease: &mut FixedSourceLease<E>,
    ) -> usize {
        let mut offered = 0;
        while let Some(observed) = self.try_recv_for(transition) {
            lease.offer(
                observed.metadata.source.participant_source(),
                observed.metadata.sequence,
                observed.observed_at,
                observed.body,
            );
            offered += 1;
        }
        offered
    }

    /// Drain commands for a pre-transition Active boundary through the typed
    /// source lease. Select the result with
    /// `lease.live_host(boundary.local_instant())` immediately before entering
    /// the native transition.
    pub fn drain_at(
        &self,
        boundary: &ActiveBoundaryStamp,
        lease: &mut FixedSourceLease<E>,
    ) -> usize {
        let mut offered = 0;
        while let Some(observed) = self.try_recv_at(boundary) {
            lease.offer(
                observed.metadata.source.participant_source(),
                observed.metadata.sequence,
                observed.observed_at,
                observed.body,
            );
            offered += 1;
        }
        offered
    }

    /// Discard every retained command, returning how many values were cleared.
    pub fn flush(&self) -> usize {
        let mut discarded = 0;
        while self.inner.try_recv().is_some() {
            discarded += 1;
        }
        discarded
    }

    pub fn terminal(&self) -> Option<ReceiveTerminal> {
        self.inner.terminal()
    }
}
