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
//!   generation-qualified contract name. Handles are body-typed
//!   ([`Publisher<T>`](bus::Publisher), [`Subscriber<T>`](bus::Subscriber),
//!   [`Latest<T>`](bus::Latest), [`Querier<Req, Resp>`](bus::Querier)), so the
//!   compiler - not a late check - rejects sending the wrong type on a topic.
//! - **No per-participant API version ceiling.** API versions are dated modules
//!   (`phoxal_api::y2026_1`, …), not semver crates. A participant's `Api` handle
//!   struct may mix bodies from different generations freely across its fields -
//!   compatibility is per-contract name identity, realized on the wire by the
//!   generation-qualified key (D1); there is no `schema_id`.
//! - **Participants are authored, not wired.** You write a `Config` struct, an
//!   `Api` handle struct, a state struct, and an `impl`;
//!   [`#[derive(Config)]`](derive@Config) / [`#[derive(Api)]`](derive@Api) plus
//!   [`#[phoxal::service|driver|simulator|tool]`](macro@service) and
//!   [`#[phoxal::behavior]`](macro@behavior) derive the static metadata, and
//!   [`run`] turns the type into a binary. Use `service` for ordinary robot
//!   participants, `driver` for a participant launched once per
//!   `robot.components` entry, `tool` for host-side utilities, and `simulator`
//!   for simulation-only participants.
//!
//! ## Author a participant
//!
//! A participant is a `Config` struct, an `Api` struct of typed bus handles, a
//! state struct, and one annotated inherent `impl`. This is the whole
//! getting-started surface:
//!
//! ```ignore
//! use phoxal_api::y2026_1;
//! use phoxal::prelude::*;
//!
//! #[derive(serde::Deserialize, phoxal::Config)]
//! struct Config {}
//!
//! #[derive(phoxal::Api)]
//! struct Api {
//!     state:  Latest<y2026_1::drive::State>,    // keep-last view of the drive state
//!     target: Publisher<y2026_1::drive::Target>, // commanded drive target
//! }
//!
//! #[phoxal::service(id = "avoid-obstacles")]
//! struct AvoidObstacles;
//!
//! #[phoxal::behavior]
//! impl AvoidObstacles {
//!     #[setup]
//!     async fn setup(ctx: &mut SetupContext<Self>, _config: Self::Config) -> Result<(Self, Self::Api)> {
//!         Ok((Self, Self::Api {
//!             state:  ctx.latest(y2026_1::topic::new().drive().state()).await?,
//!             target: ctx.publisher(y2026_1::topic::new().drive().target()).await?,
//!         }))
//!     }
//!
//!     #[step(hz = 50)]
//!     async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
//!         let now = step.time();
//!         api.target.publish_at(now, y2026_1::drive::Target {
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
//! - `use phoxal_api::y2026_1;` brings the dated api-version module into scope;
//!   `Api` struct fields name version-qualified bodies (`y2026_1::drive::Target`)
//!   directly, so a participant may mix generations across fields with no
//!   version-ceiling attribute to keep in sync.
//! - `#[derive(phoxal::Api)]` derives the bus-facing contract surface from the
//!   `Api` struct's handle fields ([`Publisher<T>`](bus::Publisher),
//!   [`Latest<T>`](bus::Latest), [`Subscriber<T>`](bus::Subscriber),
//!   [`Querier<Req, Resp>`](bus::Querier), `Server<Req, Resp>`).
//! - `#[phoxal::service(id = "…")]` links the participant state struct to its
//!   `Config`/`Api` types and records its identity.
//! - All handles are built in `#[setup]` from api-local topic builders
//!   (`y2026_1::topic::new().drive().state()`) and returned as the `Api` value
//!   alongside the participant state.
//! - `#[step(hz = ...)]` is the scheduled control loop; the runner owns timing and
//!   delivers logical time via [`StepContext`](participant::StepContext), and
//!   `&mut Self::Api` alongside `&mut self`. Query servers use `#[server]` /
//!   `#[server_snapshot]`, and `#[shutdown]` runs graceful cleanup before the bus
//!   closes.
//! - `fn main() -> phoxal::Result<()> { phoxal::run::<R>() }` is the default
//!   blocking entrypoint. For a custom Tokio main, call
//!   [`phoxal::tokio::run::<R>().await`](tokio::run).
//!
//! The four authoring kinds share the same metadata path but describe different
//! runtime roles:
//!
//! - [`macro@service`] is the ordinary typed participant surface.
//! - [`macro@driver`] is launched once per `robot.components` entry. Only a
//!   driver can call
//!   [`SetupContextDriverExt::component`](participant::SetupContextDriverExt::component)
//!   to read the bound component instance.
//! - [`macro@tool`] is for host-side utilities that inspect the robot model
//!   through
//!   [`SetupContextApiExt::robot`](participant::SetupContextApiExt::robot).
//!   Privileged raw-bus access lives under [`raw`] so it is never part of the
//!   default checked participant surface.
//! - [`macro@simulator`] is a normal participant for simulation-only processes.
//!   It carries a distinct kind and marker for simulation clock ownership.
//!
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

pub mod catalog;
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
        ApiVersion, AskQuery, BusError, BusMetadata, Codec, CodecError, CodecId, ContractBody,
        DEFAULT_QUERY_TIMEOUT, Latest, LogicalTime, MessagePack, OwnerCap, Publish, Publisher,
        Querier, QueryCode, QueryError, QueryFailure, Received, Result, ServeQuery, ServerResult,
        Source, Subscribe, Subscriber, Topic, TopicKind, TopicRole, WildcardPublish,
        encoding_string,
    };
}

/// Explicit raw/permissive bus surface for privileged participants, tooling,
/// bridges, and framework tests.
///
/// Importing this module is the conscious opt-in. The ordinary
/// `phoxal::prelude::*` and [`bus`] module do not expose raw session/open APIs.
/// `Tool` participants are emitted as `participant_class = "privileged"`; the
/// graph checker still includes their contracts, but never lets their raw
/// access satisfy checked topology.
pub mod raw {
    pub use crate::participant::runner::run_with_bus;
    pub use phoxal_bus::*;
}

/// The framework result type (`anyhow`-backed). Authoring code uses bare
/// `Result<T>` via the [`prelude`].
pub use anyhow::Result;

/// The bare `#[phoxal::behavior]` attribute for a participant's inherent impl.
pub use phoxal_macros::behavior;

/// Derive the bus-facing contract surface from an `Api` handle struct's
/// fields. See `phoxal::participant::api`.
pub use phoxal_macros::Api;

/// Derive participant config identity from a `Config` struct (schema
/// materialization is a later slice - see
/// `phoxal::participant::api::ParticipantConfig`).
pub use phoxal_macros::Config;

/// Link a participant state struct to its `Config`/`Api` types as a checked
/// service.
pub use phoxal_macros::service;

/// Link a participant state struct to its `Config`/`Api` types as a
/// component driver.
pub use phoxal_macros::driver;

/// Link a participant state struct to its `Config`/`Api` types as a
/// simulation participant.
pub use phoxal_macros::simulator;

/// Link a participant state struct to its `Config` as a raw-bus tool (`Api`
/// defaults to `()` - tools stay raw-bus only).
pub use phoxal_macros::tool;

#[doc(inline)]
pub use phoxal_macros::phoxal_api_tree;

/// Run a participant (`#[phoxal::service|driver|simulator|tool]` +
/// `#[phoxal::behavior]`) to completion on a framework-owned blocking Tokio
/// runtime.
///
/// This is the default binary entrypoint:
/// `fn main() -> phoxal::Result<()> { phoxal::run::<Participant>() }`.
pub use participant::run;

/// Async host runner entrypoint for custom Tokio mains
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
        LogicalTime, ManagedTaskPolicy, Server, SetupContext, SetupContextApiExt,
        SetupContextDriverExt, SetupContextSimulatorExt, SetupContextToolExt, ShutdownContext,
        Snapshot, StepContext,
    };
}
