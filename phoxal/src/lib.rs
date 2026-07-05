//! # phoxal
//!
//! A production-oriented framework for autonomous robots.
//!
//! Phoxal gives a robot a small, strongly-typed core: a contract bus over
//! [Zenoh](https://zenoh.io), a single dated API version per robot graph, and a
//! participant authoring model where one struct plus a couple of attribute
//! macros is a complete service, driver, tool, or simulator. The framework owns the
//! awkward parts - argument parsing, bus connection, scheduling, query serving,
//! shutdown, and health - so the code you write is the robot's behavior, not its
//! plumbing.
//!
//! Three ideas hold it together:
//!
//! - **A typed contract bus.** Every message is a plain serde body bound to one
//!   contract family and one API version. Handles are body-typed
//!   ([`Publisher<T>`](bus::Publisher), [`Subscriber<T>`](bus::Subscriber),
//!   [`Latest<T>`](bus::Latest), [`Querier<Req, Resp>`](bus::Querier)), so the
//!   compiler - not a late check - rejects sending the wrong type on a topic.
//! - **One dated API version per participant.** API versions are dated modules
//!   (`phoxal_api::y2026_1`, …), not semver crates. A participant authors against
//!   exactly one of them; mixing bodies from two versions in one participant is a
//!   compile error. A graph may mix generations: compatibility is per-contract
//!   `schema_id` agreement ([`check`](crate::check), #16).
//! - **Participants are authored, not wired.** You write a struct and an `impl`;
//!   [`#[derive(Service)]`](derive@Service), [`#[derive(Driver)]`](derive@Driver),
//!   [`#[derive(Tool)]`](derive@Tool), or
//!   [`#[derive(Simulator)]`](derive@Simulator) plus
//!   [`#[phoxal::behavior]`](macro@behavior) derives the static metadata, and
//!   [`run`] turns the type into a binary. Use `Service` for ordinary robot
//!   participants, `Driver` for a participant launched once per
//!   `components.instances` entry, `Tool` for host-side utilities, and
//!   `Simulator` for simulation-only participants that will own simulation clock
//!   duties in the later clock slice.
//!
//! ## Author a participant
//!
//! A service is one struct of typed handles and one annotated inherent `impl`.
//! The struct declares the contracts it uses (as handle fields) and its one API
//! version; the `impl` declares the lifecycle. This is the whole getting-started
//! surface:
//!
//! ```ignore
//! use phoxal_api::y2026_1 as api;   // select ONE dated API version
//! use phoxal::prelude::*;
//!
//! #[derive(phoxal::Service)]
//! #[phoxal(id = "avoid-obstacles", api = y2026_1)]
//! struct AvoidObstacles {
//!     state:  Latest<api::drive::State>,    // keep-last view of the drive state
//!     target: Publisher<api::drive::Target>, // commanded drive target
//! }
//!
//! #[phoxal::behavior]
//! impl AvoidObstacles {
//!     #[setup]
//!     async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
//!         Ok(Self {
//!             // api-local topic builders bind each handle to this API version
//!             state:  ctx.subscribe(api::topic::new().drive().state()).latest().await?,
//!             target: ctx.publisher(api::topic::new().drive().target()).await?,
//!         })
//!     }
//!
//!     #[step(hz = 50)]
//!     async fn step(&mut self, step: StepContext) -> Result<()> {
//!         let now = step.time();
//!         self.target.publish_at(now, api::drive::Target {
//!             linear_x_mps: 0.2,
//!             angular_z_radps: 0.0,
//!             curvature_limit_radpm: None,
//!         }).await?;
//!         Ok(())
//!     }
//! }
//!
//! fn main() -> phoxal::Result<()> { phoxal::run::<AvoidObstacles>() }
//! ```
//!
//! What each piece does:
//!
//! - `use phoxal_api::y2026_1 as api;` and `#[phoxal(api = y2026_1)]` pick the
//!   participant's single API version. Every handle body type comes through `api::…`;
//!   switching versions is a one-line edit at the top plus the attribute.
//! - Handle fields name version-local bodies ([`Publisher<api::drive::Target>`](bus::Publisher),
//!   [`Latest<api::drive::State>`](bus::Latest)). A body from another API version
//!   does not compile.
//! - All handles are built in `#[setup]` from api-local topic builders
//!   (`api::topic::new().drive().state()`); long-lived ones become struct fields.
//! - `#[step(hz = ...)]` is the scheduled control loop; the runner owns timing and
//!   delivers logical time via [`StepContext`](participant::StepContext). Query servers
//!   use `#[server]` / `#[server_snapshot]`, and `#[shutdown]` runs graceful
//!   cleanup before the bus closes.
//! - `fn main() -> phoxal::Result<()> { phoxal::run::<R>() }` is the default
//!   blocking entrypoint. For a custom Tokio main, call
//!   [`phoxal::tokio::run::<R>().await`](tokio::run).
//!
//! The four authoring kinds share the same metadata path but describe different
//! runtime roles:
//!
//! - `Service` is the ordinary typed participant surface.
//! - `Driver` is launched once per `components.instances` entry. Only a driver can
//!   call [`SetupContext::component`](participant::SetupContext::component) to read
//!   the bound component instance.
//! - `Tool` is for host-side utilities that inspect the robot model through
//!   [`SetupContext::robot`](participant::SetupContext::robot). Privileged raw-bus
//!   access lives under [`raw`] so it is never part of the default checked
//!   participant surface.
//! - `Simulator` is a normal participant for simulation-only processes. It carries
//!   a distinct kind and marker now; simulation clock ownership lands with plan
//!   #09.
//!
//! The runner also exposes an `emit-apis` subcommand
//! (`cargo run --example runtime_control_loop emit-apis`) that prints a participant's
//! static metadata as one JSON document and exits, without opening the bus.
//! Worked examples live in `phoxal/examples/`.
//!
//! ## Where to look next
//!
//! - The `phoxal-api` crate (`phoxal_api::y2026_1`, …) - the dated API-version
//!   modules: version-local wire bodies, the [`ApiVersion`](bus::ApiVersion) /
//!   [`ContractBody`](bus::ContractBody) traits, and the api-local topic builders,
//!   all generated by [`phoxal_api_tree!`](macro@phoxal_macros::phoxal_api_tree).
//!   A participant imports it directly with `use phoxal_api::y2026_1 as api;`.
//!   The runner also links it for framework-owned out-of-band infrastructure
//!   contracts such as bus logs.
//! - [`prelude`] - everything a participant author imports with
//!   `use phoxal::prelude::*;`: the handle types, [`SetupContext`](participant::SetupContext) /
//!   [`StepContext`](participant::StepContext), and [`Result`].
//! - [`mod@participant`] - the authoring surface behind the macros: the static metadata
//!   traits, the contexts, the clock and scheduler, and the runner
//!   ([`run`] / [`tokio::run`]).
//! - [`bus`] - the typed contract vocabulary normal participants need: the
//!   key scheme, MessagePack codec, [`BusMetadata`](bus::BusMetadata) attachment,
//!   body-typed handles, and side-branded [`Topic`](bus::Topic) values.
//! - [`raw`] - the explicit privileged/tooling surface for opening a raw bus,
//!   accessing the underlying session, or embedding runtimes on a caller-owned
//!   bus.
//! - [`model`] - the authored manifest schemas (`robot.yaml`, `structure.urdf`,
//!   `component.yaml`, …) that participants and the CLI parse.
//! - The **official service set** ships alongside this crate in the workspace
//!   `service/` tree (`drive`, `localize`, `map`, `safety`, …): full platform
//!   participants authored on exactly this surface, useful as reference reading.

// Generated macro output refers to the framework as `::phoxal::…`; make that path
// resolve to this crate so the engine's participant derives and
// `#[phoxal::behavior]` macro work when invoked inside the engine (e.g. the
// crate's own tests), the
// same as in downstream service crates. Only the in-crate test build units expand
// macros to `::phoxal::…`, so the alias is needed only under `cfg(test)`; gating
// it there keeps the non-test build free of an unused `extern crate` (no need for
// an `allow(unused_extern_crates)`).
#[cfg(test)]
extern crate self as phoxal;

pub mod check;
pub mod model;
pub mod participant;
pub mod util;

/// Typed contract and handle vocabulary for normal participant authoring.
///
/// This module deliberately excludes the raw session-owning bus types
/// (`Bus`, `BusConfig`, `BusHealth`, `IncomingQuery`, `ServerQueryable`). Checked
/// participants build IO through [`participant::SetupContext`] and the api-local
/// topic builders; privileged tools, bridges, and framework tests that need raw
/// access use [`raw`] instead.
pub mod bus {
    pub use phoxal_bus::{
        ApiVersion, AskQuery, BUS_ABI, BusAbi, BusError, BusMetadata, Codec, CodecError, CodecId,
        ContractBody, DEFAULT_QUERY_TIMEOUT, Latest, LogicalTime, MessagePack, OwnerCap, Publish,
        Publisher, Querier, QueryCode, QueryError, QueryFailure, Received, Result, ServeQuery,
        ServerResult, Source, Subscribe, Subscriber, Topic, TopicKind, TopicRole, WildcardPublish,
        encoding_string,
    };
}

/// Explicit raw/permissive bus surface for privileged participants, tooling,
/// bridges, and framework tests.
///
/// Importing this module is the conscious opt-in. The ordinary
/// `phoxal::prelude::*` and [`bus`] module do not expose raw session/open APIs.
/// `Tool` participants are emitted as `participant_class = "privileged"`; the
/// graph checker still includes their schema metadata, but never lets their raw
/// access satisfy checked topology.
pub mod raw {
    pub use crate::participant::runner::run_with_bus;
    pub use phoxal_bus::*;
}

/// The framework result type (`anyhow`-backed). Authoring code uses bare
/// `Result<T>` via the [`prelude`].
pub use anyhow::Result;

/// Derive the static metadata for a driver struct. See the crate docs.
pub use phoxal_macros::Driver;

/// Derive the static metadata for a service struct. See the crate docs.
pub use phoxal_macros::Service;

/// Derive the static metadata for a simulator struct. See the crate docs.
pub use phoxal_macros::Simulator;

/// Derive the static metadata for a tool struct. See the crate docs.
pub use phoxal_macros::Tool;

/// The bare `#[phoxal::behavior]` attribute for a participant's inherent impl.
pub use phoxal_macros::behavior;

#[doc(inline)]
pub use phoxal_macros::phoxal_api_tree;

/// Re-exported so participant config types can derive/implement
/// `phoxal::schemars::JsonSchema`, which feeds `emit-apis` config schemas.
pub use schemars;

/// Run a participant to completion on a framework-owned blocking Tokio runtime.
///
/// This is the default binary entrypoint:
/// `fn main() -> phoxal::Result<()> { phoxal::run::<Participant>() }`.
pub use participant::run;

/// Async host runner entrypoints for custom Tokio mains
/// (`phoxal::tokio::run::<Participant>().await`).
pub mod tokio {
    #[doc(inline)]
    pub use crate::participant::run_async as run;
}

/// Everything a participant author imports with `use phoxal::prelude::*;`.
pub mod prelude {
    pub use crate::Result;
    pub use crate::bus::{Latest, Publisher, Querier, QueryError, ServerResult, Subscriber};
    pub use crate::participant::{
        LogicalTime, ManagedTaskPolicy, SetupContext, ShutdownContext, Snapshot, StepContext,
    };
}
