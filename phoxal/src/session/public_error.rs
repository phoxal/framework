//! Public session-client failures.

/// A failure while using an established public session.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The generated Protobuf-over-Zenoh public session exchange failed.
    #[error(transparent)]
    Public(#[from] crate::communication_transport::PublicTransportError),

    /// A handle no longer refers to the live logical session or timeline.
    #[error("the public session handle is stale: {resource}")]
    StaleHandle {
        /// Resource whose session identity is no longer current.
        resource: &'static str,
    },

    /// A remotely advertised descriptor did not match the generated local
    /// descriptor supplied to `Service::port`.
    #[error("the public port descriptor is not admitted: {detail}")]
    PortNotAdmitted {
        /// Bounded descriptor diagnostic.
        detail: String,
    },

    /// A local public-session configuration or operation argument is invalid.
    #[error("invalid public session request: {detail}")]
    InvalidPublicRequest {
        /// Bounded local diagnostic.
        detail: String,
    },
}
