//! # phoxal
//!
//! A production-oriented framework for autonomous robots.
//!
//! Phoxal gives a robot a small, strongly-typed core: a contract bus over
//! [Zenoh](https://zenoh.io), framework-owned semantic API contracts,
//! and a
//! participant authoring model where a role marker plus a direct trait
//! implementation is a complete service, driver, or simulator. The framework owns the
//! awkward parts - argument parsing, bus connection, scheduling, query serving,
//! shutdown, and health - so the code you write is the robot's behavior, not its
//! plumbing.
//!
//! Three ideas hold it together:
//!
//! - **A typed contract bus.** Every message is a plain serde body bound to one
//!   family-rooted contract name. Handles are endpoint-typed
//!   ([`StatePublisher<T>`](bus::StatePublisher),
//!   [`StateView<T>`](bus::StateView), [`SampleReceiver<T>`](bus::SampleReceiver),
//!   [`Querier<Req, Resp>`](bus::Querier)), so the compiler - not a late check -
//!   rejects sending the wrong type on a topic. Publishing is additionally
//!   gated by the contract's *temporal* role: the robot time a publisher can
//!   express is fixed by what the contract is, so a participant cannot stamp an
//!   instant it never reached.
//! - **One authoring API facade.** Official participants import `phoxal::api`,
//!   the complete `robot` contract family. Contract identity is realized on the
//!   wire by the family-rooted key; compatibility between participants is the
//!   framework train version they were built from.
//! - **Participants are authored, not wired.** A role attribute declares
//!   identity and associated `Config`/`State`/`Api` types; a direct
//!   [`Participant`] implementation owns lifecycle
//!   behavior, and [`run`] turns the marker into a binary. Use `service` for ordinary robot
//!   participants, `driver` for a participant launched once per
//!   `robot.components` entry, `simulator` for simulation-only
//!   participants, and `brain` for the robot project's one mandatory
//!   composition root.
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
//!     state:  StateView<api::endpoint::drive::StateEndpoint>, // keep-last drive state
//!     target: SetpointPublisher<api::endpoint::drive::TargetEndpoint>, // commanded drive target
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
//!             state:  ctx.state_view(api::topic::client().drive().state()).await?,
//!             target: ctx.setpoint_publisher(api::topic::client().drive().target())?,
//!         }))
//!     }
//!
//!     #[phoxal::step(hz = 50)]
//!     fn step(
//!         &self,
//!         api: &Self::Api,
//!         _step: StepContext,
//!         _state: &mut Self::State,
//!     ) -> Result<()> {
//!         api.target.send(api::drive::Target::try_new(0.2, 0.0)?)?;
//!         Ok(())
//!     }
//! }
//!
//! fn main() -> phoxal::Result<()> { phoxal::run::<AvoidObstacles>() }
//! ```
//!
//! What each piece does:
//!
//! - `use phoxal::api;` brings the `robot` contract family into scope;
//!   `Api` struct fields name its bodies (`api::drive::Target`) directly, with
//!   no participant-local contract attribute to keep in sync.
//! - The role attribute records identity and sets associated types. Omitted
//!   `Config`, `State`, and `Api` default to `()`.
//! - Handles are ordinary fields built in `Participant::setup` from typed topic
//!   builders and returned alongside mutable state.
//! - `#[phoxal::step(hz = ...)]` adds a cadence to the trait's step override.
//! - `ctx.query(owner_endpoint, Self::handler)` registers typed query handlers;
//!   the endpoint fixes the handler's request and response types at compile time,
//!   and the runner supplies trusted requester [`QueryContext`] provenance.
//! - The runner serializes step, query, reset, and shutdown access to `State`.
//! - `fn main() -> phoxal::Result<()> { phoxal::run::<R>() }` is the default
//!   blocking entrypoint. For a custom Tokio main, call
//!   [`phoxal::tokio::run::<R>().await`](tokio::run).
//!
//! The four authoring kinds share the same metadata path but describe
//! different runtime roles:
//!
//! - [`macro@service`] is the ordinary typed participant surface.
//! - [`macro@driver`] is launched once per `robot.components` entry. Only a
//!   driver can call
//!   [`SetupContext::component`]
//!   to read the bound component instance.
//! - [`macro@simulator`] is a normal participant for simulation-only processes.
//!   It carries a distinct kind and marker for simulation clock ownership.
//! - [`macro@brain`] is the robot project's one mandatory composition root:
//!   the root Cargo package's binary, staged as `bin/brain`. Its identity is
//!   fixed to `brain` and its `Config` is always `()`; it owns mission and
//!   intent policy as ordinary Rust code and holds no capability a service
//!   does not. It is never declared under `robot.yaml` `services:`.
//!
//! Worked examples live in `phoxal/examples/`.
//!
//! ## Where to look next
//!
//! - The `phoxal-api` crate (`phoxal::api`, …) - the contract-family
//!   modules: family-local wire bodies, the [`ApiFamily`](bus::ApiFamily) /
//!   endpoint descriptor traits and family-local topic builders, all generated
//!   from modular `phoxal_api_tree!` and `phoxal_api_fragment!` declarations.
//!   A participant imports it directly with `use phoxal::api as api;`.
//!   The runner also links it for framework-owned out-of-band infrastructure
//!   contracts such as bus logs.
//! - [`prelude`] - everything a participant author imports with
//!   `use phoxal::prelude::*;`: the handle types, [`SetupContext`],
//!   [`StepContext`], and [`Result`].
//! - [`bus`] - the typed contract vocabulary normal participants need: the
//!   key scheme, MessagePack codec, [`BusMetadata`](bus::BusMetadata) attachment,
//!   the four non-interchangeable time types, endpoint-typed handles, and
//!   side-branded [`Topic`](bus::Topic) values.
//! - [`model`] - immutable canonical runtime robot facts supplied from the
//!   finalized `runtime.json`; bundle assembly and host-side reading live in
//!   `phoxal-bundle`, while authored document readers live in
//!   `phoxal-manifest` as a build/source dependency only.
//! - [`geometry`] and [`SampleSchedule`] - the small shared arithmetic every
//!   official participant would otherwise reimplement.
//! - The **official service set** ships alongside this crate in the workspace
//!   `services/` tree (`drive`, `localize`, `map`, `safety`, …): full platform
//!   participants authored on exactly this surface, useful as reference reading.

// Generated macro output refers to the framework as `::phoxal::…`; make that path
// resolve to this crate so role/config macros work inside the engine's own tests, the
// same as in downstream service crates. Only the in-crate test build units expand
// macros to `::phoxal::…`, so the alias is needed only under `cfg(test)`; gating
// it there keeps the non-test build free of an unused `extern crate` (no need for
// an `allow(unused_extern_crates)`).
#[cfg(test)]
extern crate self as phoxal;

pub mod geometry;
mod participant;
mod sample_schedule;

/// The contract surface this crate owns: the participant launch contract.
///
/// Not public API. It exists so compatibility CI can read this crate's declared
/// process boundary out of the crate itself.
#[doc(hidden)]
pub mod __compat;

/// Explicit in-process participant testing support.
#[cfg(feature = "test-harness")]
pub mod testing;

/// The `robot` contract family, the surface a participant authors against.
///
/// The facade exposes the robot family only. The `runtime` and `supervisor`
/// families are host-tooling surfaces, reached through `phoxal_api` directly.
pub mod api {
    pub use phoxal_api::robot::*;
}

/// Typed contract and handle vocabulary for normal participant authoring.
///
/// This is the bus surface a checked participant browses: the contract traits,
/// the codec, the [`BusMetadata`](bus::BusMetadata) attachment, the four
/// non-interchangeable time types, the endpoint-typed handles, and side-branded
/// [`Topic`](bus::Topic) values. Participants build their IO through
/// [`SetupContext`] and the api-local topic builders, so session construction,
/// ownership, raw handles, incoming queries, and server queryables have no
/// place here. Host tooling that owns a session depends on `phoxal-bus`
/// directly; a participant cannot open a second session through this facade.
///
/// [`TimelineAuthority`](phoxal_bus::TimelineAuthority) and
/// [`WorldClockPublisher`](phoxal_bus::WorldClockPublisher) are absent for a
/// stronger reason: they are world-clock authority, which only a
/// `#[phoxal::simulator]` may hold. A simulator reaches them through its
/// role-gated [`SetupContext`] methods and nowhere else, so keeping them off
/// the browsable surface leaves exactly one route to them. See
/// [`TimelineAuthority`](phoxal_bus::TimelineAuthority)'s own docs for how
/// strong that guarantee is.
pub mod bus {
    pub use phoxal_bus::{
        ApiFamily, AskQuery, BusError, BusMetadata, CaptureStamp, Codec, CodecError, CodecId,
        DEFAULT_QUERY_TIMEOUT, DeliveryFamily, Endpoint, EndpointDescriptor, EndpointKind,
        EventContract, EventPublisher, EventReceiver, ExclusiveProducerLease, FixedSourceAdmission,
        FixedSourceLease, KeySegment, KeySegmentError, LEASE_TRACE_TARGET, LeaseDecision,
        LeaseRejection, LocalInstant, MAX_READY_PRODUCERS, MessagePack, Observed, ParticipantId,
        ParticipantReadyEvent, ParticipantReadyEvents, ParticipantReadyObserver,
        ParticipantReadyStatus, ParticipantSourceIdentity, Payload, ProducerId, Publish, Querier,
        QueryCode, QueryEndpointDescriptor, QueryError, QueryFailure, QueryResult, ReceiveTerminal,
        Result, RobotInstant, RobotTimeError, SampleContract, SampleDeliveryContract,
        SamplePublisher, SampleReceiver, ServeQuery, SetpointContract, SetpointDeliveryContract,
        SetpointPublisher, SetpointReceiver, SourceAttribution, SourceLabel, StateContract,
        StateDeliveryContract, StatePublisher, StateView, StepStamp, StepToken, StreamContract,
        StreamDeliveryContract, StreamPublisher, StreamReceiver, Subscribe, TimeWindow, Timed,
        TimelineId, TimelineMismatch, Topic, TopicKind, WallTimestamp, WildcardPublish,
        WorldClockContract, WorldStepToken,
    };
}

/// The canonical runtime robot model a [`phoxal_bundle::RuntimeBundle`] yields.
///
/// This mirrors `phoxal-model`'s own facade one-for-one and adds nothing: the
/// names below are the canonical ones, and everything else is reached through
/// the module that owns it ([`model::builder`], [`model::component`],
/// [`model::identity`], [`model::robot`], [`model::simulation`],
/// [`model::structure`]). [`AssetId`] is the logical identity shared by source
/// compilation and the bundle index. Participant asset access is the
/// bundle-owned, digest-checked [`ParticipantAssetResolver`] capability below;
/// source compilation does not cross this runtime boundary.
///
/// [`model::RobotBuilder`] composes a model in memory rather than loading one.
/// A launched participant never needs it - the runner hands it an already-built
/// [`model::Robot`] - but a test or a tool that has no bundle does.
pub mod model {
    pub use phoxal_model::{
        CapabilityRole, Clock, FootprintEnvelope, IdentifierKind, JointOwner, KinematicScalarField,
        LinkRole, ModelError, MotionLimitField, PoseOwner, Robot, RobotBuilder, StructureError,
        builder, component, footprint, identity, robot, simulation, structure,
    };
}

/// The framework result type (`anyhow`-backed). Authoring code uses bare
/// `Result<T>` via the [`prelude`].
pub use anyhow::Result;

/// Derive a participant config's compile-time JSON Schema from a `Config`
/// struct.
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

/// Declare the one mandatory root brain, the robot project's composition root.
///
/// Fixed identity `brain` and `Config = ()`; otherwise exactly the checked
/// service surface.
pub use phoxal_macros::brain;

/// Attach a cadence to `Participant::step`.
pub use phoxal_macros::step;

/// Run a participant to completion on a framework-owned blocking Tokio runtime.
///
/// This is the default binary entrypoint:
/// `fn main() -> phoxal::Result<()> { phoxal::run::<Participant>() }`.
pub use participant::runner::run;

pub use participant::api::Participant;
pub use participant::context::{QueryContext, ResetContext, SetupContext, StepContext};
pub use participant::managed::ManagedTaskPolicy;
pub use phoxal_bundle::ParticipantAssets as ParticipantAssetResolver;
pub use phoxal_model::AssetId;
pub use sample_schedule::{MissedTickPolicy, SampleSchedule};

/// Async host runner entrypoint for custom Tokio mains
/// (`phoxal::tokio::run::<Participant>().await`).
pub mod tokio {
    #[doc(inline)]
    pub use crate::participant::runner::run_async as run;
}

/// Everything a participant author imports with `use phoxal::prelude::*;`.
pub mod prelude {
    pub use crate::Result;
    pub use crate::bus::{
        CaptureStamp, EventPublisher, EventReceiver, ExclusiveProducerLease, FixedSourceAdmission,
        FixedSourceLease, LeaseDecision, LocalInstant, Observed, Querier, QueryError, QueryResult,
        RobotInstant, SampleDeliveryContract, SamplePublisher, SampleReceiver,
        SetpointDeliveryContract, SetpointPublisher, SetpointReceiver, StateDeliveryContract,
        StatePublisher, StateView, StreamDeliveryContract, StreamReceiver, TimeWindow, Timed,
        TimelineId,
    };
    pub use crate::{
        AssetId, ManagedTaskPolicy, Participant, ParticipantAssetResolver, QueryContext,
        ResetContext, SetupContext, StepContext,
    };
}

/// The macro ABI: the exact set of items the code `phoxal-macros` generates
/// has to be able to name inside a participant's own crate.
///
/// This is not public API. Nothing here carries a stability guarantee, nothing
/// here is documented for authors, and the only code allowed to name any of it
/// is a `#[phoxal::service]` / `driver` / `simulator` / `brain` / `step` /
/// `#[derive(phoxal::Config)]` expansion. Every item is listed explicitly and
/// individually below: a glob re-export here would silently publish the whole
/// participant engine as public API, so the list is the boundary.
///
/// A participant author reaches the same concepts through the crate root, the
/// [`prelude`], and [`bus`]. If something an author needs is only reachable
/// from here, that is a missing facade entry, not a licence to import this
/// module.
#[doc(hidden)]
pub mod __private {
    /// The compatibility declaration a participant binary carries.
    ///
    /// The framework train version is the whole of it: two Phoxal processes
    /// speak the same contracts exactly when they were built from the same
    /// train, so there is one constant here and it is owned by
    /// `phoxal-runtime-contract`. The role macros splice it into the
    /// participant's embedded `.phoxal_meta` document at compile time through
    /// `participant_metadata_json!`; that embedded document is the only
    /// compatibility artifact - there is no Cargo package-metadata table and no
    /// version file. The document's own grammar is the tag on
    /// `ParticipantMetadata` itself, a format discriminator rather than a
    /// negotiated identity, so it needs no entry here. Topology requirements
    /// are a closed typed declaration; they are not inferred from package names
    /// or a service registry.
    pub mod compatibility {
        use phoxal_runtime_contract::metadata::ParticipantRequirement;
        use phoxal_runtime_contract::version::FrameworkVersion;

        /// The canonical spelling of the framework train this binary was built
        /// from. The const-eval metadata writer needs a string; the value it
        /// spells is `FrameworkVersion::CURRENT`.
        pub const FRAMEWORK: &str = FrameworkVersion::CURRENT_SPELLING;
        /// The default declaration for participants without a static topology
        /// requirement.
        pub const NO_REQUIREMENT: Option<ParticipantRequirement> = None;
        /// The stock differential-drive participant's declared topology and
        /// motor-command requirement.
        pub const STOCK_DRIVE_REQUIREMENT: Option<ParticipantRequirement> =
            Some(ParticipantRequirement::DifferentialDriveVelocity);

        pub use phoxal_runtime_contract::participant_metadata_json;
    }

    /// Const-eval plumbing for the embedded metadata static: `ConstSchema`,
    /// `bytes_of`, and the hygienic `concatcp` re-export.
    pub use crate::participant::config::meta;

    /// The capability marker traits a role attribute implements for its marker.
    pub use crate::participant::surface;

    /// The traits a role attribute and `#[derive(phoxal::Config)]` implement.
    pub use crate::participant::config::ParticipantConfig;
    pub use crate::participant::spec::ParticipantSpec;

    /// The authoring kind a role attribute records in `ParticipantSpec::KIND`.
    pub use phoxal_runtime_contract::metadata::{ParticipantKind, ParticipantRequirement};

    /// The cadence `#[phoxal::step(hz = …)]` returns from
    /// `Participant::__step_schedule`.
    pub use crate::participant::scheduler::StepSchedule;
}
