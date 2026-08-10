//! The framework-owned wire-contract catalogue: every payload that crosses a
//! Phoxal process boundary and the semantic endpoint that carries it, grouped
//! into families.
//!
//! Payload structs, enums, implementations, and tests are ordinary Rust items
//! in family-first modules. A sibling [`phoxal_api_fragment!`] declares only
//! that module's endpoints. Payloads own serde shape, construction invariants,
//! and domain behavior; they do not know their topic or delivery policy.
//! [`phoxal_api_tree!`] materializes deterministic descriptors and typed topic
//! builders, then re-exports the authored payloads through each family.
//!
//! A **family** is the first path segment of every fragment and the leading
//! segment of every key it declares: it names a semantic contract namespace,
//! not a revision. There are three:
//!
//! - [`robot`] - the robot domain a participant authors against. `phoxal::api`
//!   re-exports this family and only this one.
//! - [`runtime`] - facts a running Phoxal process emits about itself: its log
//!   events, its bus and step telemetry, and the authoritative simulation
//!   clock. Any process publishes here; the family names no collector.
//! - [`supervisor`] - the wire vocabulary a supervisor speaks. `phoxald`, the
//!   CLI's supervisor daemon, owns supervisor state and behavior; this crate
//!   owns only what an answer looks like.
//!
//! Compatibility is owned entirely by the framework train version each
//! participant binary embeds, compared for exact equality, so no key,
//! descriptor, or body carries a per-API version. The one exception is
//! [`supervisor::connect`], the frozen bootstrap two binaries exchange before
//! they know whether their trains agree.
//!
//! Endpoint semantics are fixed by the declaration: `State`, `Sample`, `Event`,
//! `Stream`, `Setpoint`, or bounded query. Source identity, robot/capture time,
//! ordered positions, loss, gaps, and terminal evidence remain bus metadata and
//! never become generated fields in a domain payload.

mod api;
pub use api::generated::*;
pub use phoxal_macros::{phoxal_api_fragment, phoxal_api_fragment_group, phoxal_api_tree};
