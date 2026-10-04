//! Common lifecycle state used by generated runtime bindings.
//!
//! Contract-specific dispatch borrows these resources. Only the runtime
//! owner's explicit acceptance or discard commits or retires staged work.

use std::sync::atomic::{AtomicU64, Ordering};

use super::behavior::BehaviorDiary;
use super::capture::CaptureRegistry;
use super::{ContextResources, PendingCalls, PendingOwner, next_execution_epoch};

/// One runtime's call ownership, captures, diagnostics, and execution fence.
///
/// Generated bindings own this value; authored runtimes use their ordinary
/// invocation context and do not construct lifecycle state themselves.
#[derive(Default)]
pub struct AuthoringState {
    pending: PendingCalls,
    captures: CaptureRegistry,
    diary: BehaviorDiary,
    epoch: AtomicU64,
}

impl AuthoringState {
    /// Reinitializes the execution and fences every prior ticket and capture.
    pub fn reset(&self) {
        self.epoch.store(next_execution_epoch(), Ordering::Relaxed);
        self.pending.clear();
        self.captures.clear();
        self.diary.clear();
    }

    /// The initialization's process-wide unique execution epoch.
    #[must_use]
    pub fn execution_epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }

    /// Borrows the same lifecycle resources for every handler and step.
    #[must_use]
    pub fn resources(&self) -> ContextResources<'_> {
        ContextResources {
            pending: &self.pending,
            captures: &self.captures,
            diary: &self.diary,
        }
    }

    /// Records the candidate's direct-handler and behavior-tree call owners.
    pub fn stage_calls(
        &self,
        direct: impl IntoIterator<Item = (u128, &'static str)>,
        trees: impl IntoIterator<Item = (u128, u64)>,
    ) -> crate::Result<()> {
        for (ticket, field) in direct {
            self.pending.stage(ticket, PendingOwner::Direct(field))?;
        }
        for (ticket, generation) in trees {
            self.pending
                .stage(ticket, PendingOwner::Tree { generation })?;
        }
        Ok(())
    }

    /// Commits staged work after the owner admits the complete candidate.
    pub fn accepted(&self) {
        self.pending.promote_staged();
        self.diary.promote_staged();
    }

    /// Retires staged work when the owner rejects the candidate.
    pub fn discarded(&self) {
        self.pending.discard_staged();
        self.diary.discard_staged();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discarded_calls_never_commit_on_a_later_acceptance() {
        let state = AuthoringState::default();
        state.reset();
        state
            .stage_calls([(1, "start")], [(2, 7)])
            .expect("stage calls");
        assert_eq!(state.resources().pending.owner_of(1), None);
        state.discarded();
        state
            .stage_calls([(3, "next")], [])
            .expect("stage next candidate");
        state.accepted();
        state.accepted();
        let pending = state.resources().pending;
        assert_eq!(pending.owner_of(1), None);
        assert_eq!(pending.owner_of(2), None);
        assert_eq!(pending.owner_of(3), Some(PendingOwner::Direct("next")));
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn reset_retires_resources_and_allocates_a_distinct_epoch() {
        let first = AuthoringState::default();
        let second = AuthoringState::default();
        first.reset();
        second.reset();
        let epoch = first.execution_epoch();
        assert_ne!(epoch, second.execution_epoch());
        first.stage_calls([], [(1, 7)]).expect("stage tree call");
        first.accepted();
        assert!(first.resources().captures.activate(1, "events", 0));
        assert!(second.resources().pending.is_empty());
        assert_eq!(second.resources().captures.active_count(), 0);
        first.reset();
        assert_ne!(epoch, first.execution_epoch());
        assert!(first.resources().pending.is_empty());
        assert_eq!(first.resources().captures.active_count(), 0);
    }
}
