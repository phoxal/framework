//! Structured session and operation failures.
//!
//! The three failures stay separate because a caller can act differently on
//! each: a connect failure is a decision about whether to retry or report, an
//! operation failure is about one request, and a close failure is evidence
//! about a session that is already over.

use crate::bus::{BusError, BusFault, QueryError, SourceLabelError};
use crate::identity::ExecutionId;
use crate::supervisor::api::execution::SnapshotError;
use crate::version::FrameworkVersion;

/// Which peer is on the newer incompatible framework line.
///
/// This is a fact established from the two exact versions, not advice about
/// how an application should resolve it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatibilityRefusal {
    /// The remote supervisor is newer than this session.
    RemoteNewer,
    /// This session is newer than the remote supervisor.
    LocalNewer,
}

impl std::fmt::Display for CompatibilityRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RemoteNewer => formatter.write_str("remote framework line is newer"),
            Self::LocalNewer => formatter.write_str("local framework line is newer"),
        }
    }
}

/// A failure while establishing one session.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The configured endpoint announced no execution.
    #[error("no Phoxal execution is reachable at {endpoint}")]
    NoExecution { endpoint: String },

    /// The configured endpoint announced more than one execution and therefore
    /// did not identify one connection target.
    #[error(
        "{count} Phoxal executions are reachable at {endpoint}, which must identify exactly one: {executions:?}"
    )]
    MultipleExecutions {
        endpoint: String,
        count: usize,
        executions: Vec<ExecutionId>,
    },

    /// The diagnostic label could not be represented by the framework bus.
    #[error(transparent)]
    SourceLabel(#[from] SourceLabelError),

    /// The peers were built from incompatible framework lines.
    #[error("remote framework {remote} is incompatible with local framework {local}: {refusal}")]
    IncompatibleFramework {
        remote: FrameworkVersion,
        local: FrameworkVersion,
        refusal: CompatibilityRefusal,
    },

    /// The frozen bootstrap returned a document this session could not decode.
    #[error("the frozen supervisor bootstrap reply could not be decoded: {detail}")]
    UnreadableBootstrap { detail: String },

    /// The supervisor identity was already absent when setup completed.
    #[error("the supervisor identity was lost while the session was being established")]
    SupervisorUnavailable,

    /// The initial supervisor snapshot was internally inconsistent.
    #[error("the supervisor returned an invalid initial snapshot: {0}")]
    Snapshot(#[from] SnapshotError),

    /// The underlying transport failed while the session was opening.
    #[error(transparent)]
    Bus(#[from] BusError),

    /// A bootstrap or initial-state query failed.
    #[error(transparent)]
    Query(#[from] QueryError),
}

impl ConnectError {
    /// Whether the peer answered the frozen bootstrap but could not be admitted
    /// as a compatible framework peer.
    #[must_use]
    pub const fn is_compatibility_refusal(&self) -> bool {
        matches!(
            self,
            Self::IncompatibleFramework { .. } | Self::UnreadableBootstrap { .. }
        )
    }
}

impl From<crate::execution::BootstrapError> for ConnectError {
    fn from(error: crate::execution::BootstrapError) -> Self {
        match error {
            crate::execution::BootstrapError::NoExecution { endpoint } => {
                Self::NoExecution { endpoint }
            }
            crate::execution::BootstrapError::MultipleExecutions {
                endpoint,
                count,
                executions,
            } => Self::MultipleExecutions {
                endpoint,
                count,
                executions,
            },
            crate::execution::BootstrapError::IncompatibleFramework {
                remote,
                local,
                refusal,
            } => Self::IncompatibleFramework {
                remote,
                local,
                refusal: match refusal {
                    crate::execution::CompatibilityRefusal::RemoteNewer => {
                        CompatibilityRefusal::RemoteNewer
                    }
                    crate::execution::CompatibilityRefusal::LocalNewer => {
                        CompatibilityRefusal::LocalNewer
                    }
                },
            },
            crate::execution::BootstrapError::UnreadableBootstrap { detail } => {
                Self::UnreadableBootstrap { detail }
            }
            crate::execution::BootstrapError::Bus(error) => Self::Bus(error),
            crate::execution::BootstrapError::Query(error) => Self::Query(error),
        }
    }
}

/// The terminal fact that ended an established session.
///
/// The first observed reason is latched for the session's lifetime. A later
/// close request cannot overwrite an earlier supervisor, snapshot, or transport
/// failure.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DisconnectReason {
    /// The unique session owner explicitly closed or was dropped.
    SessionClosed,
    /// The supervisor's execution-scoped identity token was lost.
    SupervisorIdentityLost,
    /// The authoritative supervisor snapshot stream failed.
    SnapshotStreamFailed { detail: String },
    /// An owner-owned transport worker failed.
    TransportFault { fault: BusFault },
    /// The private lifecycle channel ended without publishing a cause.
    LifecycleEnded,
}

impl std::fmt::Display for DisconnectReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionClosed => formatter.write_str("the session owner closed"),
            Self::SupervisorIdentityLost => {
                formatter.write_str("the supervisor identity token was lost")
            }
            Self::SnapshotStreamFailed { detail } => {
                write!(formatter, "the supervisor snapshot stream failed: {detail}")
            }
            Self::TransportFault { fault } => write!(formatter, "transport fault: {fault}"),
            Self::LifecycleEnded => {
                formatter.write_str("the session lifecycle ended without a terminal cause")
            }
        }
    }
}

/// A failure while using an established session.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The established session reached a terminal state.
    #[error("the session ended: {reason}")]
    Disconnected { reason: DisconnectReason },

    /// The underlying typed transport operation failed.
    #[error(transparent)]
    Bus(#[from] BusError),

    /// A typed query failed.
    #[error(transparent)]
    Query(#[from] QueryError),
}

/// A failure while deterministically closing the unique session owner.
#[derive(Debug, thiserror::Error)]
pub enum CloseError {
    /// The bus completed close with retained transport or worker evidence.
    #[error("the session transport did not close cleanly: {detail}")]
    Transport { detail: String },

    /// The private lifecycle task failed before returning close evidence.
    #[error("the session lifecycle task failed: {detail}")]
    Lifecycle { detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(remote: FrameworkVersion, local: FrameworkVersion) -> ConnectError {
        crate::execution::ensure_compatible_framework(remote, local)
            .map_err(ConnectError::from)
            .expect_err("different lines are incompatible")
    }

    #[test]
    fn compatibility_refusal_preserves_versions_and_which_peer_is_newer() {
        let older = FrameworkVersion::new(0, 60, 4);
        let newer = FrameworkVersion::new(0, 61, 2);

        assert!(matches!(
            refusal(newer, older),
            ConnectError::IncompatibleFramework {
                remote,
                local,
                refusal: CompatibilityRefusal::RemoteNewer,
            } if remote == newer && local == older
        ));
        assert!(matches!(
            refusal(older, newer),
            ConnectError::IncompatibleFramework {
                remote,
                local,
                refusal: CompatibilityRefusal::LocalNewer,
            } if remote == older && local == newer
        ));
    }

    #[test]
    fn compatibility_errors_are_neutral_structured_facts() {
        let error = refusal(
            FrameworkVersion::new(0, 61, 0),
            FrameworkVersion::new(0, 60, 0),
        );
        let rendered = error.to_string();
        assert!(rendered.contains("0.61.0"), "{rendered}");
        assert!(rendered.contains("0.60.0"), "{rendered}");
        assert!(
            rendered.contains("remote framework line is newer"),
            "{rendered}"
        );
    }
}
