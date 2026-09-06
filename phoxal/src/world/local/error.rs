use super::*;

#[derive(Debug, thiserror::Error)]
pub enum WorldSessionWireError {
    #[error("world-session I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("world-session encoding failed: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("world-session decoding failed: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("world-session frame is {bytes} bytes, exceeding the {maximum}-byte bound")]
    FrameTooLarge { bytes: usize, maximum: usize },
    #[error("invalid loopback world-session endpoint '{endpoint}'")]
    InvalidEndpoint { endpoint: String },
    #[error("remote framework {remote} is incompatible with local framework {local}")]
    IncompatibleFramework {
        local: FrameworkVersion,
        remote: FrameworkVersion,
    },
    #[error("world host refused the operation: {0}")]
    Refused(String),
    #[error("invalid world-session state: {0}")]
    State(#[from] crate::world::api::session::state::WorldSessionStateError),
    #[error("invalid world-session diagnostics: {0}")]
    Diagnostics(#[from] crate::world::api::session::diagnostics::WorldSessionDiagnosticsError),
    #[error("world-session {operation} timed out after {timeout_ms} ms")]
    Timeout { operation: String, timeout_ms: u64 },
    #[error("world-session protocol failed: {0}")]
    Protocol(String),
    #[error("world-session state contradicts frozen bootstrap field '{field}'")]
    BootstrapMismatch { field: &'static str },
    #[error("the world-session stream closed")]
    Closed,
    #[error(
        "the world-session {stream} stream lost {skipped} replacement(s); query current and resubscribe"
    )]
    StreamGap { stream: &'static str, skipped: u64 },
}
