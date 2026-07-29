//! # phoxal
//!
//! A production-oriented framework for autonomous robots.
//!
//! Phoxal gives a robot a small, strongly-typed core: a contract bus over
//! [Zenoh](https://zenoh.io), train-selected concrete API contracts,
//! and a
//! participant authoring model where a role marker plus a direct trait
//! implementation is a complete service, driver, tool, or simulator. The framework owns the
//! awkward parts - argument parsing, bus connection, scheduling, query serving,
//! shutdown, and health - so the code you write is the robot's behavior, not its
//! plumbing.
//!
//! Three ideas hold it together:
//!
//! - **A typed contract bus.** Every message is a plain serde body bound to one
//!   version-qualified contract name. Handles are body-typed
//!   ([`StatePublisher<T>`](bus::StatePublisher),
//!   [`Subscriber<T>`](bus::Subscriber), [`Latest<T>`](bus::Latest),
//!   [`Querier<Req, Resp>`](bus::Querier)), so the compiler - not a late check -
//!   rejects sending the wrong type on a topic. Publishing is additionally
//!   gated by the contract's *temporal* role: the robot time a publisher can
//!   express is fixed by what the contract is, so a participant cannot stamp an
//!   instant it never reached.
//! - **One train-selected API facade.** Official participants import
//!   `phoxal::api`, which names the complete concrete revision selected by the
//!   locked framework train. Contract identity is realized on the wire by the
//!   revision-qualified key (D1).
//! - **Participants are authored, not wired.** A role attribute declares
//!   identity and associated `Config`/`State`/`Api` types; a direct
//!   [`Participant`](participant::Participant) implementation owns lifecycle
//!   behavior, and [`run`] turns the marker into a binary. Use `service` for ordinary robot
//!   participants, `driver` for a participant launched once per
//!   `robot.components` entry, `tool` for host-side utilities, and `simulator`
//!   for simulation-only participants.
//!
//! ## Author a participant
//!
//! A participant is a unit role marker, optional `Config`/`State`/`Api` types,
//! and one direct trait implementation:
//!
//! ```ignore
//! use phoxal::api;
//! use phoxal::prelude::*;
//!
//! struct Api {
//!     state:  Latest<api::drive::State>,            // keep-last view of the drive state
//!     target: CommandPublisher<api::drive::Target>, // commanded drive target
//! }
//!
//! #[phoxal::service(id = "avoid-obstacles", api = Api)]
//! struct AvoidObstacles;
//!
//! impl Participant for AvoidObstacles {
//!     async fn setup(
//!         &self,
//!         ctx: &mut SetupContext<Self>,
//!         _config: Self::Config,
//!     ) -> Result<(Self::State, Self::Api)> {
//!         Ok(((), Api {
//!             state:  ctx.latest(api::topic::client().drive().state()).await?,
//!             target: ctx.command_publisher(api::topic::client().drive().target()).await?,
//!         }))
//!     }
//!
//!     #[phoxal::step(hz = 50)]
//!     async fn step(
//!         &self,
//!         api: &Self::Api,
//!         _step: StepContext,
//!         _state: &mut Self::State,
//!     ) -> Result<()> {
//!         api.target.send(api::drive::Target {
//!             linear_x_mps: 0.2,
//!             angular_z_radps: 0.0,
//!             curvature_limit_radpm: None,
//!         })?;
//!         Ok(())
//!     }
//! }
//!
//! fn main() -> phoxal::Result<()> { phoxal::run::<AvoidObstacles>() }
//! ```
//!
//! What each piece does:
//!
//! - `use phoxal::api;` brings the versioned API module into scope;
//!   `Api` struct fields name train-selected bodies (`api::drive::Target`)
//!   directly, with no participant-local version attribute to keep in sync.
//! - The role attribute records identity and sets associated types. Omitted
//!   `Config`, `State`, and `Api` default to `()`.
//! - Handles are ordinary fields built in `Participant::setup` from typed topic
//!   builders and returned alongside mutable state.
//! - `#[phoxal::step(hz = ...)]` adds a cadence to the trait's step override.
//! - `ctx.query(owner_endpoint, Self::handler)` registers typed query handlers;
//!   the endpoint fixes the handler's request and response types at compile time.
//! - The runner serializes step, query, reset, and shutdown access to `State`.
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
//!   [`SetupContextToolExt::robot`](participant::SetupContextToolExt::robot).
//!   Privileged raw-bus access lives under [`raw`] so it is never part of the
//!   default checked participant surface. A tool joins the execution, not the
//!   clock: it can observe and command, and nothing on its surface hands it a
//!   [`RobotInstant`](bus::RobotInstant) - see [`raw`]'s docs for where that
//!   boundary is a compiler rule and where it is a convention.
//! - [`macro@simulator`] is a normal participant for simulation-only processes.
//!   It carries a distinct kind and marker for simulation clock ownership.
//!
//! Worked examples live in `phoxal/examples/`.
//!
//! ## Where to look next
//!
//! - The `phoxal-api` crate (`phoxal::api`, …) - the versioned API
//!   modules: version-local wire bodies, the [`ApiVersion`](bus::ApiVersion) /
//!   [`ContractBody`](bus::ContractBody) traits, and the api-local topic builders,
//!   all generated by [`phoxal_api_tree!`](macro@phoxal_macros::phoxal_api_tree).
//!   A participant imports it directly with `use phoxal::api as api;`.
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
//!   the four non-interchangeable time types, body-typed handles, and
//!   side-branded [`Topic`](bus::Topic) values.
//! - [`raw`] - the explicit privileged/tooling surface for opening a raw bus,
//!   accessing the underlying session, or embedding runtimes on a caller-owned
//!   bus.
//! - [`model`] - the authored manifest schemas (`robot.yaml`, `structure.urdf`,
//!   `component.yaml`, …) that participants and the CLI parse.
//! - The **official service set** ships alongside this crate in the workspace
//!   `service/` tree (`drive`, `localize`, `map`, `safety`, …): full platform
//!   participants authored on exactly this surface, useful as reference reading.

// Generated macro output refers to the framework as `::phoxal::…`; make that path
// resolve to this crate so role/config macros work inside the engine's own tests, the
// same as in downstream service crates. Only the in-crate test build units expand
// macros to `::phoxal::…`, so the alias is needed only under `cfg(test)`; gating
// it there keeps the non-test build free of an unused `extern crate` (no need for
// an `allow(unused_extern_crates)`).
#[cfg(test)]
extern crate self as phoxal;

pub mod behavior;
pub mod check;
pub mod model;
pub mod participant;
pub mod util;

/// The concrete framework API revision selected by this release train.
pub use phoxal_api::latest as api;

/// Typed contract and handle vocabulary for normal participant authoring.
///
/// This module deliberately excludes the raw session-owning bus types
/// (`Bus`, `BusConfig`, `BusHealth`, `IncomingQuery`, `ServerQueryable`), and
/// also [`TimelineAuthority`](phoxal_bus::TimelineAuthority) and
/// [`WorldClockPublisher`](phoxal_bus::WorldClockPublisher): both are world-
/// clock-authority types only a `#[phoxal::simulator]` ever legitimately
/// names, so keeping them off the surface an ordinary participant browses
/// closes the accidental route - see
/// [`TimelineAuthority`](phoxal_bus::TimelineAuthority)'s docs for the exact
/// strength of that claim. Checked participants build IO through
/// [`participant::SetupContext`] and the api-local topic builders; privileged
/// tools, bridges, framework tests, and the one simulator that legitimately
/// names these two types use [`raw`] instead.
pub mod bus {
    pub use phoxal_bus::{
        ApiVersion, AskQuery, BusError, BusMetadata, CaptureStamp, Codec, CodecError, CodecId,
        CommandContract, CommandPublisher, ContractBody, DEFAULT_QUERY_TIMEOUT, DiagnosticContract,
        DiagnosticPublisher, ExecutionId, LEASE_TRACE_TARGET, Latest, Lease, LeaseDecision,
        LeaseRejection, LocalInstant, MeasurementContract, MeasurementPublisher, MessagePack,
        Observed, ProducerFence, ProducerId, Publish, Querier, QueryCode, QueryError, QueryFailure,
        QueryResult, Result, RobotInstant, ServeQuery, StateContract, StatePublisher, StepStamp,
        StepToken, Subscribe, Subscriber, TimeWindow, TimelineId, TimelineMismatch, Topic,
        TopicKind, TopicRole, WallTimestamp, WildcardPublish, WorldClockContract, WorldStepToken,
        encoding_string,
    };

    /// Bus-ABI golden bindings against the train-selected API tree.
    ///
    /// The pure bus mechanics (encoding-string parsing, the codec fast-reject
    /// in `decode_sample`, codec round-trips, namespace validation) are unit
    /// tested in the `phoxal-bus` crate against a hand-written `ContractBody`.
    /// These pin the *engine-level* binding: that the `phoxal_api_tree!`
    /// generated `ContractBody`/`ApiVersion` impls for `v0_1` flow through the
    /// re-exported wire slots above exactly as published, and that `TOPIC` is
    /// version-qualified (D1).
    #[cfg(test)]
    mod tests {
        use super::{
            BusMetadata, CodecId, ContractBody, ProducerId, RobotInstant, TimeWindow, TimelineId,
            encoding_string,
        };
        use crate::api;

        #[test]
        fn encoding_string_carries_only_the_codec() {
            let enc = encoding_string(CodecId::MessagePack);
            assert_eq!(enc, "phoxal/v0;codec=1");
        }

        #[test]
        fn contract_body_topic_is_version_qualified_on_the_real_tree() {
            // D1: the revision is folded into the wire key, so a real v0.1
            // contract publishes on a key that cannot collide with a
            // hypothetical v0.2 contract of the same leaf name.
            assert_eq!(
                <api::drive::Target as ContractBody>::TOPIC,
                "v0.1/drive/target"
            );
            assert_eq!(
                <api::asset::GetRequest as ContractBody>::TOPIC,
                "v0.1/asset/get"
            );
        }

        #[test]
        fn bus_metadata_for_a_real_body_round_trips() {
            let timeline = TimelineId::mint();
            let meta = BusMetadata {
                codec: CodecId::MessagePack.as_u8(),
                producer: ProducerId::mint(),
                sequence: 9,
                produced_at: Some(TimeWindow::exact(RobotInstant::new(timeline, 42))),
                participant: "tester".to_string(),
            };
            assert_eq!(BusMetadata::decode(&meta.encode()).unwrap(), meta);

            // A command or diagnostic expresses no robot time, and that absence
            // round trips as absence rather than as a zero instant.
            let timeless = BusMetadata {
                produced_at: None,
                ..meta
            };
            let decoded = BusMetadata::decode(&timeless.encode()).unwrap();
            assert_eq!(decoded, timeless);
            assert_eq!(decoded.produced_exactly_at(), None);
        }
    }
}

/// Explicit raw/permissive bus surface for privileged participants, tooling,
/// bridges, and framework tests.
///
/// Importing this module is the conscious opt-in. The ordinary
/// `phoxal::prelude::*` and [`bus`] module do not expose raw session/open APIs,
/// and - for the same reason - do not name
/// [`TimelineAuthority`](phoxal_bus::TimelineAuthority) or
/// [`WorldClockPublisher`](phoxal_bus::WorldClockPublisher) either; this is
/// also where the one simulator that legitimately owns the world clock
/// imports those two from. `Tool` participants are marked
/// `participant_class = "privileged"` so runtime diagnostics can identify
/// that deliberate boundary crossing.
///
/// # Robot time: what the types enforce, and what they do not
///
/// Raw access reaches the session, the subscription handles, the querier, and
/// the command/diagnostic publishers - everything a tool or bridge needs to
/// watch a robot and act on it. Commands carry no production instant at all, so
/// nothing here lets a tool *date* anything by publishing one.
///
/// Publishing checked state or a measurement is different: it needs a
/// [`StepToken`](phoxal_bus::StepToken), a
/// [`WorldStepToken`](phoxal_bus::WorldStepToken), or a
/// [`CaptureStamp`](phoxal_bus::CaptureStamp). The runner mints the first from
/// each step it actually reaches, and the second comes from a
/// [`TimelineAuthority`](phoxal_bus::TimelineAuthority) the world-authority
/// participant holds. In ordinary authoring there is no way to obtain either
/// one out of thin air, and the sealed [`StepStamp`](phoxal_bus::StepStamp)
/// trait plus the role markers make using the wrong one a compile error.
///
/// **That is a strong convention, not a sealed boundary, and this module will
/// not pretend otherwise.** `RobotInstant`, `StepToken`, and the role
/// publishers are defined in `phoxal-bus` while the runner that mints them
/// lives here, so their constructors have to be `pub` for the runner to call
/// them - and Rust has no visibility level between "this crate" and "the
/// world". They are `#[doc(hidden)]` and named to be conspicuous
/// (`__mint`), which makes fabricating robot time something a participant can
/// only do on purpose, in code that says so. The alternative - folding the api,
/// the bus, and the runtime into one crate so the constructors could be
/// `pub(crate)` - buys a compiler-enforced guarantee at the cost of the crate
/// split; it is a deliberate open question, not an oversight (#952 section D).
pub mod raw {
    pub use crate::participant::runner::{run_with_bus, run_with_bus_clock};
    pub use phoxal_bus::*;
}

/// The framework result type (`anyhow`-backed). Authoring code uses bare
/// `Result<T>` via the [`prelude`].
pub use anyhow::Result;

/// SemVer of the single framework train this facade was built from.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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

/// Attach a cadence to `Participant::step`.
pub use phoxal_macros::step;

/// Run a participant to completion on a framework-owned blocking Tokio runtime.
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
    pub use crate::bus::{
        CaptureStamp, CommandPublisher, DiagnosticPublisher, Latest, Lease, LeaseDecision,
        LocalInstant, MeasurementPublisher, Observed, ProducerFence, Querier, QueryError,
        QueryResult, RobotInstant, StatePublisher, Subscriber, TimeWindow, TimelineId,
    };
    pub use crate::participant::{
        ManagedTaskPolicy, Participant, ResetContext, SetupContext, SetupContextApiExt,
        SetupContextDriverExt, SetupContextSimulatorExt, SetupContextToolExt, StepContext,
    };
}
