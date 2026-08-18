//! The supervisor boundary.
//!
//! The framework-owned `phoxal-supervisor` process owns supervisor state and
//! behavior. What lives here is what everything else has to agree with it
//! about: [`api`], the wire vocabulary a supervisor speaks, and
//! [`rendezvous`], the host paths and advisory locking through which a client
//! and the one execution supervisor find and fence each other.

/// The `supervisor` contract family.
pub mod api {
    pub use crate::protocol::supervisor::*;
}

pub mod rendezvous;
