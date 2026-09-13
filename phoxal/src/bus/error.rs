//! Errors at the private Zenoh owner boundary.

use std::fmt;

/// Result type for the private transport owner.
pub(crate) type Result<T> = std::result::Result<T, BusError>;

/// A failure opening, using, or closing the private transport session.
#[derive(Debug)]
pub(crate) enum BusError {
    /// The owner or one of its handles has already closed.
    Closed,
    /// A Zenoh operation failed.
    Transport(String),
    /// The opened session did not retain the producer identity requested by
    /// the owner.
    SessionIdentityMismatch { expected: String, observed: String },
    /// A router session did not retain the execution identity requested by the
    /// supervisor.
    #[cfg(feature = "supervisor")]
    ExecutionIdentityMismatch { expected: String, observed: String },
    /// A Zenoh session identity was not a valid Phoxal identity.
    ForeignSessionId { value: String, role: &'static str },
    /// The session identity sequence cannot advance further.
    SequenceExhausted,
}

impl fmt::Display for BusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("private transport session is closed"),
            Self::Transport(error) => write!(formatter, "private transport failed: {error}"),
            Self::SessionIdentityMismatch { expected, observed } => write!(
                formatter,
                "private transport session identity mismatch: expected {expected}, observed {observed}"
            ),
            #[cfg(feature = "supervisor")]
            Self::ExecutionIdentityMismatch { expected, observed } => write!(
                formatter,
                "router identity mismatch: expected {expected}, observed {observed}"
            ),
            Self::ForeignSessionId { value, role } => {
                write!(formatter, "foreign {role} session identity `{value}`")
            }
            Self::SequenceExhausted => formatter.write_str("private transport sequence exhausted"),
        }
    }
}

impl std::error::Error for BusError {}
