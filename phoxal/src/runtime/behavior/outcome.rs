//! Typed tick outcomes and the fallback-eligibility policy.
//!
//! The taxonomy keeps expected domain refusal, known pre-admission
//! rejection, local expiry, unknown remote outcome, and integrity failure
//! distinguishable. A selector may try its next branch only after an
//! explicitly eligible failure; an oversized or uncertain result followed
//! an already-executed remote effect and never authorizes another
//! potentially conflicting request.

use super::TreeStatus;

/// One tick's node-level outcome: still running, succeeded (optionally
/// carrying one owned payload), or ended with a terminal tree status.
/// Private execution machinery behind the composition surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::runtime::behavior) enum Step<T = ()> {
    Running,
    Succeeded,
    /// Succeeded while handing one owned payload to the immediate parent.
    SucceededWith(T),
    Ended {
        status: TreeStatus,
        /// The retained reason a terminal status was reached.
        cause: Option<String>,
        /// The typed failure classification a selector consults.
        kind: FailureKind,
    },
}

/// Why a node ended terminally, and whether another branch may follow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureKind {
    /// The node has not failed.
    None,
    /// An expected domain refusal: the remote answered and the domain —
    /// through its response predicate or refusal response — declined.
    /// The remote effect, if any, is known and bounded by the domain's own
    /// semantics, so a selector may try its next branch.
    DomainRefusal,
    /// The request was rejected before admission: no remote effect was
    /// produced, so a selector may try its next branch.
    RejectedBeforeAdmission,
    /// A local deadline expired or a response arrived too large to decode
    /// after the remote effect may already have executed. Another branch
    /// could conflict with the unobserved effect, so a selector stops.
    UncertainEffect,
}

impl FailureKind {
    /// Whether a selector may advance to its next branch after a child
    /// ended with this classification.
    #[must_use]
    pub const fn fallback_eligible(self) -> bool {
        matches!(
            self,
            FailureKind::DomainRefusal | FailureKind::RejectedBeforeAdmission
        )
    }
}

/// Maps one completion failure to the outcome taxonomy, retaining the
/// exact cause and the fallback classification.
pub(in crate::runtime::behavior) fn outcome_of(error: crate::runtime::input::RequestError) -> Step {
    use crate::runtime::input::RequestError;
    match error {
        RequestError::NotSent(_) => Step::Ended {
            status: TreeStatus::Refused,
            cause: Some("the call was not sent".to_owned()),
            kind: FailureKind::RejectedBeforeAdmission,
        },
        RequestError::RejectedBeforeAdmission(detail) => Step::Ended {
            status: TreeStatus::Refused,
            cause: Some(format!("rejected before admission: {detail}")),
            kind: FailureKind::RejectedBeforeAdmission,
        },
        RequestError::Oversized => Step::Ended {
            status: TreeStatus::Refused,
            cause: Some(
                "the response exceeded the admitted byte bound after the remote effect may have \
                 executed"
                    .to_owned(),
            ),
            kind: FailureKind::UncertainEffect,
        },
        RequestError::Timeout => Step::Ended {
            status: TreeStatus::TimedOut,
            cause: Some("the call's response deadline elapsed".to_owned()),
            kind: FailureKind::UncertainEffect,
        },
        RequestError::OutcomeUnknown(detail) => Step::Ended {
            status: TreeStatus::TimedOut,
            cause: Some(format!("the remote outcome stayed unknown: {detail}")),
            kind: FailureKind::UncertainEffect,
        },
        // Integrity failures fault the invocation before this taxonomy;
        // the mapping keeps the match exhaustive if the surface widens.
        RequestError::Integrity(detail) => Step::Ended {
            status: TreeStatus::Failed,
            cause: Some(format!("the response failed integrity: {detail}")),
            kind: FailureKind::UncertainEffect,
        },
    }
}
