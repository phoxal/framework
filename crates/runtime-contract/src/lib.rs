//! Stable data crossing Phoxal process boundaries.
//!
//! This crate deliberately contains no participant runner, bus transport,
//! command-line parser, or project compiler. It is the shared vocabulary used
//! by the framework runtime and `phoxal-cli`.
//!
//! There is no crate-root facade: every public item is reached through the
//! module that owns its contract, so a type has exactly one path and an import
//! already says which process-boundary contract it belongs to.
//!
//! - [`identity`] - the execution, producer, and timeline identities that reach
//!   the wire.
//! - [`version`] - the version identities two binaries compare to establish
//!   that they speak the same contracts.
//! - [`metadata`] - the record every participant binary embeds at compile time,
//!   and its strict parser.
//! - [`emit`] - the one sanctioned writer of that record, in both of its
//!   evaluation modes.
//! - [`origin`] - the boot-anchored origin of one real execution.
//! - [`wire_schema`] - the deterministic model of the shapes those contracts
//!   put on the wire, which compatibility CI checks against published
//!   baselines.

pub mod clock;
pub mod emit;
pub mod identity;
pub mod metadata;
pub mod origin;
pub mod version;
pub mod wire_schema;
