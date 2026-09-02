//! # phoxal
//!
//! A production-oriented framework for autonomous robots.
//!
//! Phoxal gives a robot a small, strongly-typed core: a contract bus over
//! [Zenoh](https://zenoh.io), framework-owned semantic API contracts,
//! and a
//! participant authoring model where a role marker plus a direct trait
//! implementation is a complete service or driver. The framework owns the
//! awkward parts - argument parsing, bus connection, scheduling, query serving,
//! shutdown, and health - so the code you write is the robot's behavior, not its
//! plumbing.
//!
//! Three ideas hold it together:
//!
//! - **A typed contract bus.** Every message is a plain serde body bound to one
//!   family-rooted contract name, and that body *is* the endpoint. Handles are
//!   endpoint-typed ([`StatePublisher<E>`](bus::StatePublisher),
//!   [`StateView<E>`](bus::StateView), [`SampleReceiver<E>`](bus::SampleReceiver),
//!   [`Querier<E>`](bus::Querier)), so the compiler - not a late check -
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
//!   `Participant` implementation owns lifecycle
//!   behavior, and `phoxal::run` turns the marker into a binary. Use `service` for ordinary robot
//!   participants, `driver` for a participant launched once per
//!   `robot.components` entry, and `brain` for the robot project's one
//!   mandatory composition root.
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
//!     state:  StateView<api::drive::State>,       // keep-last drive state
//!     target: SetpointPublisher<api::drive::Target>, // commanded drive target
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
//!             state:  ctx.state_view(api::topics().drive().state().client()).await?,
//!             target: ctx.setpoint_publisher(api::topics().drive().target().client())?,
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
//!   `Api` struct fields name its bodies (`api::drive::Target`) directly - the
//!   body is the endpoint, so there is no second descriptor identity and no
//!   participant-local contract attribute to keep in sync.
//! - The role attribute records identity and sets associated types. Omitted
//!   `Config`, `State`, and `Api` default to `()`.
//! - Handles are ordinary fields built in `Participant::setup` from typed topic
//!   builders and returned alongside mutable state.
//! - `#[phoxal::step(hz = ...)]` adds a cadence to the trait's step override.
//! - `ctx.query(owner_endpoint, Self::handler)` registers typed query handlers;
//!   the endpoint fixes the handler's request and response types at compile time,
//!   and the runner supplies trusted requester `QueryContext` provenance.
//! - The runner serializes step, query, reset, and shutdown access to `State`.
//! - `fn main() -> phoxal::Result<()> { phoxal::run::<R>() }` is the default
//!   blocking entrypoint. For a custom Tokio main, call
//!   `phoxal::tokio::run::<R>().await`.
//!
//! The three authoring kinds share the same metadata path but describe
//! different runtime roles:
//!
//! - `#[phoxal::service]` is the ordinary typed participant surface.
//! - `#[phoxal::driver]` is launched once per driven `robot.components` entry,
//!   under that entry's own id. Only a driver can call
//!   `SetupContext::component` to read the component it is bound to, or
//!   `SetupContext::connection` to read how that component is wired to the
//!   machine. The entry's `driver:` block is two slots with one owner each: a
//!   `connection` from the framework's closed
//!   [`model::connection`] vocabulary, and a `config`
//!   the driver binary alone gives shape to.
//! - `#[phoxal::brain]` is the robot project's one mandatory composition root:
//!   the root Cargo package's binary, staged as `bin/brain`. Its identity is
//!   fixed to `brain` and its `Config` is always `()`; it owns mission and
//!   intent policy as ordinary Rust code and holds no capability a service
//!   does not. It is never declared under `robot.yaml` `services:`.
//!
//! Worked examples live in `phoxal/examples/`.
//!
//! ## Where to look next
//!
//! - `phoxal::api` - the `robot` contract family: the wire bodies and the
//!   dynamic topic tree they are declared in, module by module (each module a
//!   branch of child nodes or a leaf of endpoints, never both). A participant
//!   imports it directly with `use phoxal::api;` and walks it from
//!   `api::topics()`. The runner also uses the sibling
//!   `runtime` family for framework-owned out-of-band infrastructure contracts
//!   such as bus logs, which a participant never names itself.
//! - `phoxal::prelude` - everything a participant author imports with
//!   `use phoxal::prelude::*;`: the handle types, `SetupContext`,
//!   `StepContext`, and [`Result`].
//! - [`bus`] - the typed contract vocabulary normal participants need: the
//!   key scheme, MessagePack codec, [`BusMetadata`](bus::BusMetadata) attachment,
//!   the four non-interchangeable time types, endpoint-typed handles, and
//!   side-branded [`Topic`](bus::Topic) values.
//! - [`model`] - immutable canonical robot facts read from the bundle's
//!   `manifest.json`. Bundle assembly and host-side reading are
//!   `phoxal::bundle`, and the authored document readers are
//!   `phoxal::authoring`; both are host-role surfaces a launched participant
//!   never reaches.
//! - [`geometry`] and [`SampleSchedule`] - the small shared arithmetic every
//!   official participant would otherwise reimplement.
//! - The **official service set** ships alongside this crate in the workspace
//!   `services/` tree (`drive`, `localize`, `map`, `safety`, …): full platform
//!   participants authored on exactly this surface, useful as reference reading.
//!
//! ## Host SDKs
//!
//! A robot developer writes participants; the processes *around* a robot -
//! the CLI and Operator applications that attach to a running execution, an
//! external simulator that owns a world, the source compiler behind
//! `phoxal build` - are separate consumer roles. They are the same crate and
//! the same train, selected by a Cargo feature so that none of them crowds the
//! participant surface above.
//!
//! - **`session`** - `phoxal::session`: attach to one running execution.
//!   `Session` uniquely owns the transport and the lifecycle; a cloneable
//!   `SessionHandle` performs typed operations. The profile also publishes the
//!   `runtime` and `supervisor` contract families, the participant launch
//!   encoder, and the embedded participant-metadata reader.
//! - **`simulator`** - `phoxal::simulator`: stand an external world process in
//!   for a robot's component drivers. `SimulatorSession` owns typed component
//!   IO, delegated presence, and the world clock, without handing out the raw
//!   transport underneath.
//! - **`authoring`** - `phoxal::authoring`: the authored-source layer
//!   (`robot.yaml`, `component.yaml`, `simulation.yaml`, URDF), its JSON
//!   schemas, and the compiler that turns them into a [`model::Robot`]. A
//!   launched participant never reads an authored document.
//!
//! The supervisor's own implementation is `phoxal::supervisor::host`, behind
//! the `supervisor` profile and hidden from these docs: it is the body of the
//! framework-owned `phoxal-supervisor` executable, not an SDK. Everything a
//! client has to agree with it about is `phoxal::supervisor::api` and
//! `phoxal::supervisor::rendezvous`, which the `session` profile publishes.
//!
//! Every path named in this section is spelled rather than linked, because the
//! profile that publishes it is not the profile you are reading these docs in
//! unless you enabled it. docs.rs enables all of them.
//!
//! Profiles are additive compilation and visibility controls, never authority
//! boundaries: Cargo unifies features, and who may do what at runtime remains
//! process ownership and the constructible API.

// docs.rs builds with every profile enabled and this cfg set, so each
// profile-gated module carries an "available on crate feature …" badge instead
// of being missing from the published documentation.
#![cfg_attr(docsrs, feature(doc_cfg))]
// Generated macro output refers to the framework as `::phoxal::…`; make that
// path resolve to this crate so the role/config macros and the `DescribeWire`
// derive expand the same way inside the framework as they do in a downstream
// service crate.
extern crate self as phoxal;

// # Consumer profiles
//
// This file is the only place in `phoxal/src` where a Cargo feature may be
// named, and `cargo xtask policy` enforces it. A profile decides which modules
// are `pub`; it never forks a code path inside a module, because the moment it
// does the one library starts growing the old package topology back as
// scattered `#[cfg]`s.
//
// Two module trees are the stated exceptions, gated as a unit at their
// declaration below because their whole content belongs to one role and pulls
// dependencies a robot binary has no business linking: `authoring` and
// `supervisor::host`. `session`, `simulator`, `testing` and the embedded
// router join them for the narrower reason that nothing else in their own
// profile would use them, and an unused private module is dead code.

pub mod geometry;
mod sample_schedule;

// The participant engine. Its two public children are the process-boundary
// contracts a *host* reads: `metadata`, the record every participant artifact
// embeds, and `launch`, the argv a launcher writes. So the module is public
// exactly for the profiles that launch or inspect participants, and private for
// the profile that *is* one - a participant author reaches the engine through
// the crate-root facade below, and the role attributes reach it through
// `__private`, the macro ABI, which is the only path either needs.
mod execution;
#[cfg(any(feature = "session", feature = "supervisor", feature = "authoring"))]
#[cfg_attr(
    docsrs,
    doc(cfg(any(feature = "session", feature = "supervisor", feature = "authoring")))
)]
#[allow(
    dead_code,
    reason = "a host profile publishes this module for its two process contracts \
              and reaches the participant engine below them through nothing at all: \
              the engine's facade is the crate root, which only the `participant` \
              profile publishes. That profile declares this module privately and \
              without this allow, so the engine is still linted where it is alive."
)]
pub mod participant;
#[cfg(all(
    feature = "participant",
    not(any(feature = "session", feature = "supervisor", feature = "authoring"))
))]
mod participant;
#[cfg(not(any(
    feature = "participant",
    feature = "session",
    feature = "supervisor",
    feature = "authoring"
)))]
#[allow(
    dead_code,
    reason = "a build that selected no consumer role, or only the simulator role, \
              reaches neither half of this module: no crate-root facade for the \
              engine, and no public module for the two process contracts (a \
              simulator stands in for drivers but never launches or inspects a \
              participant binary). Every other profile reaches one half and lints \
              the other."
)]
mod participant;

// Each module below carries its own `//!` header, which is where its
// documentation lives; a second fragment written here would be merged into it
// from this file's scope and take every intra-doc link in it with it. What
// this file says about a module is therefore a plain comment, and it says only
// what belongs to the profile decision.
pub mod bus;

// The embedded Zenoh router the graph meets on. Its file is `bus/router.rs`,
// because the router and the participants that dial it must agree on the
// transport policy the bus pins, and because the bus module is the one place in
// this crate that may name Zenoh at all. It is declared *here* rather than by
// `bus/mod.rs` because it needs `zenoh/unstable`, which only the `supervisor`
// profile carries, and a profile gate may live nowhere but this file.
//
// Crate-private in every profile: raw fabric ownership is not an SDK.
#[cfg(feature = "supervisor")]
#[path = "bus/router.rs"]
pub(crate) mod router;

pub mod model;

// A participant reads its bundle through the runner - `ctx.robot()` and
// `ctx.assets()` - rather than by opening one, so the reader and the writer are
// the host roles' surface.
#[cfg(any(feature = "simulator", feature = "supervisor", feature = "authoring"))]
#[cfg_attr(
    docsrs,
    doc(cfg(any(feature = "simulator", feature = "supervisor", feature = "authoring")))
)]
pub mod bundle;
#[cfg(not(any(feature = "simulator", feature = "supervisor", feature = "authoring")))]
#[allow(
    dead_code,
    unused_imports,
    reason = "a profile that does not publish a tree still compiles it - the \
              compatibility aggregate reads every contract family, and the runner \
              reads the bundle - so everything in it reads as unreachable here. The \
              profile that does publish it is where these lints have something to say."
)]
mod bundle;

// Build/source tooling only. A launched participant reads the compiled
// `manifest.json`, never an authored document, so the YAML/TOML/URDF readers
// under this module are off by default.
#[cfg(feature = "authoring")]
#[cfg_attr(docsrs, doc(cfg(feature = "authoring")))]
pub mod authoring;

pub mod identity;

pub mod version;

// A participant emits its logs and telemetry through the runner and never names
// the runtime family, so the family is a host-role surface: applications that
// read a running execution and the supervisor that retains its live evidence.
#[cfg(any(feature = "session", feature = "simulator", feature = "supervisor"))]
#[cfg_attr(
    docsrs,
    doc(cfg(any(feature = "session", feature = "simulator", feature = "supervisor")))
)]
pub mod runtime;
#[cfg(not(any(feature = "session", feature = "simulator", feature = "supervisor")))]
#[allow(
    dead_code,
    unused_imports,
    reason = "a profile that does not publish a tree still compiles it - the \
              compatibility aggregate reads every contract family, and the runner \
              reads the bundle - so everything in it reads as unreachable here. The \
              profile that does publish it is where these lints have something to say."
)]
mod runtime;

// Simulation progress is a fourth semantic wire family. A participant runner
// consumes it internally, but participant-authored code never receives this
// module as a public surface. World hosts, sessions, and the supervisor
// publish or inspect it through the one canonical path below.
#[cfg(any(feature = "session", feature = "simulator", feature = "supervisor"))]
#[cfg_attr(
    docsrs,
    doc(cfg(any(feature = "session", feature = "simulator", feature = "supervisor")))
)]
pub mod simulation;
#[cfg(not(any(feature = "session", feature = "simulator", feature = "supervisor")))]
#[allow(
    dead_code,
    unused_imports,
    reason = "a participant runner consumes the simulation clock internally but does not expose its host contract family to authored code"
)]
mod simulation;

/// Declare the `supervisor` boundary at the visibility this profile gives it.
///
/// The tree is written once, here, because `supervisor::host` is gated on its
/// own profile *inside* a module whose own visibility flips - and a profile
/// gate may live nowhere but this file. Expanding one declaration twice is what
/// keeps the two visibilities from drifting apart.
macro_rules! supervisor_boundary {
    ( $( #[$attribute:meta] )* $visibility:vis mod supervisor ; ) => {
        $( #[$attribute] )*
        /// The supervisor boundary.
        ///
        /// The framework-owned `phoxal-supervisor` process owns supervisor
        /// state and behavior. What lives here is what everything else has to
        /// agree with it about: [`api`](supervisor::api), the wire vocabulary a
        /// supervisor speaks, and [`rendezvous`](supervisor::rendezvous), the
        /// host paths and advisory locking through which a client and the one
        /// execution supervisor find and fence each other.
        $visibility mod supervisor {
            pub mod api;

            pub mod rendezvous;

            // The supervisor implementation itself: the body of the
            // framework-owned `phoxal-supervisor` executable, compiled only by
            // the exact-train `supervisor` profile and documented nowhere,
            // because it is not an SDK.
            #[cfg(feature = "supervisor")]
            #[cfg_attr(docsrs, doc(cfg(feature = "supervisor")))]
            #[doc(hidden)]
            pub mod host;
        }
    };
}

#[cfg(any(feature = "session", feature = "supervisor"))]
supervisor_boundary!(
    pub mod supervisor;
);
#[cfg(not(any(feature = "session", feature = "supervisor")))]
supervisor_boundary!(
    #[allow(
        dead_code,
        unused_imports,
        reason = "a profile that does not publish a tree still compiles it - the \
                  compatibility aggregate reads every contract family - so everything \
                  in it reads as unreachable here. The profile that does publish it is \
                  where these lints have something to say."
    )]
    mod supervisor;
);

#[cfg(feature = "session")]
#[cfg_attr(docsrs, doc(cfg(feature = "session")))]
pub mod session;

#[cfg(feature = "simulator")]
#[cfg_attr(docsrs, doc(cfg(feature = "simulator")))]
pub mod simulator;

// Not public API. It exists so compatibility CI can read this crate's declared
// process boundary out of the crate itself, and it is the same aggregate in
// every profile: hiding a contract family from participant rustdoc must not
// remove it from the train.
#[doc(hidden)]
pub mod __compat;

#[cfg(feature = "test-harness")]
#[cfg_attr(docsrs, doc(cfg(feature = "test-harness")))]
pub mod testing;

#[cfg(any(feature = "participant", feature = "session", feature = "simulator"))]
#[cfg_attr(
    docsrs,
    doc(cfg(any(feature = "participant", feature = "session", feature = "simulator")))
)]
pub mod api;
#[cfg(not(any(feature = "participant", feature = "session", feature = "simulator")))]
#[allow(
    dead_code,
    unused_imports,
    reason = "a profile that does not publish a tree still compiles it - the \
              compatibility aggregate reads every contract family, and the runner \
              reads the bundle - so everything in it reads as unreachable here. The \
              profile that does publish it is where these lints have something to say."
)]
mod api;

/// The two declarations the api tree is built from, at the crate root so a
/// family module reads `crate::nodes!` / `crate::endpoints!` whatever its
/// depth. Crate-private: the api tree is framework-owned and closed.
pub(crate) use crate::bus::tree::{endpoints, nodes};

/// The framework result type (`anyhow`-backed). Authoring code uses bare
/// `Result<T>` via `phoxal::prelude`.
pub use anyhow::Result;

/// Derive a participant config's compile-time JSON Schema from a `Config`
/// struct.
#[cfg(feature = "participant")]
#[cfg_attr(docsrs, doc(cfg(feature = "participant")))]
pub use phoxal_macros::Config;

/// Link a participant state struct to its `Config`/`Api` types as a checked
/// service.
#[cfg(feature = "participant")]
#[cfg_attr(docsrs, doc(cfg(feature = "participant")))]
pub use phoxal_macros::service;

/// Link a participant state struct to its `Config`/`Api` types as a component
/// driver, and optionally declare the one connection kind it accepts.
#[cfg(feature = "participant")]
#[cfg_attr(docsrs, doc(cfg(feature = "participant")))]
pub use phoxal_macros::driver;

/// Declare the one mandatory root brain, the robot project's composition root.
///
/// Fixed identity `brain` and `Config = ()`; otherwise exactly the checked
/// service surface.
#[cfg(feature = "participant")]
#[cfg_attr(docsrs, doc(cfg(feature = "participant")))]
pub use phoxal_macros::brain;

/// Attach a cadence to `Participant::step`.
#[cfg(feature = "participant")]
#[cfg_attr(docsrs, doc(cfg(feature = "participant")))]
pub use phoxal_macros::step;

/// Run a participant to completion on a framework-owned blocking Tokio runtime.
///
/// This is the default binary entrypoint:
/// `fn main() -> phoxal::Result<()> { phoxal::run::<Participant>() }`.
#[cfg(feature = "participant")]
#[cfg_attr(docsrs, doc(cfg(feature = "participant")))]
pub use participant::runner::run;

#[cfg(feature = "participant")]
#[cfg_attr(docsrs, doc(cfg(feature = "participant")))]
pub use crate::bundle::ParticipantAssets as ParticipantAssetResolver;
pub use crate::model::AssetId;
#[cfg(feature = "participant")]
#[cfg_attr(docsrs, doc(cfg(feature = "participant")))]
pub use participant::api::Participant;
#[cfg(feature = "participant")]
#[cfg_attr(docsrs, doc(cfg(feature = "participant")))]
pub use participant::context::{QueryContext, ResetContext, SetupContext, StepContext};
#[cfg(feature = "participant")]
#[cfg_attr(docsrs, doc(cfg(feature = "participant")))]
pub use participant::managed::ManagedTaskPolicy;
pub use sample_schedule::{MissedTickPolicy, SampleSchedule};

/// Async host runner entrypoint for custom Tokio mains
/// (`phoxal::tokio::run::<Participant>().await`).
#[cfg(feature = "participant")]
#[cfg_attr(docsrs, doc(cfg(feature = "participant")))]
pub mod tokio {
    #[doc(inline)]
    pub use crate::participant::runner::run_async as run;
}

/// Everything a participant author imports with `use phoxal::prelude::*;`.
#[cfg(feature = "participant")]
#[cfg_attr(docsrs, doc(cfg(feature = "participant")))]
pub mod prelude {
    pub use crate::Result;
    pub use crate::bus::{
        CaptureStamp, EventPublisher, EventReceiver, ExclusiveProducerLease, FixedSourceAdmission,
        FixedSourceLease, LeaseDecision, LocalInstant, Observed, Querier, QueryError, QueryResult,
        RobotInstant, SamplePublisher, SampleReceiver, SetpointPublisher, SetpointReceiver,
        StatePublisher, StateView, StreamReceiver, TimeWindow, Timed, TimelineId,
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
/// is a `#[phoxal::service]` / `driver` / `brain` / `step` /
/// `#[derive(phoxal::Config)]` expansion. Every item is listed explicitly and
/// individually below: a glob re-export here would silently publish the whole
/// participant engine as public API, so the list is the boundary.
///
/// A participant author reaches the same concepts through the crate root,
/// `phoxal::prelude`, and [`bus`]. If something an author needs is only reachable
/// from here, that is a missing facade entry, not a licence to import this
/// module.
#[doc(hidden)]
pub mod __private {
    /// The compatibility declaration a participant binary carries.
    ///
    /// The framework train version is the whole of it: two Phoxal processes
    /// speak the same contracts exactly when they were built from the same
    /// compatibility line, and the exact train the binary records is what a
    /// validator reads that line from. So there is one constant here, owned by
    /// [`crate::version`]. The role macros splice it into the
    /// participant's embedded `.phoxal_meta` document at compile time through
    /// `participant_metadata_json!`; that embedded document is the only
    /// compatibility artifact - there is no Cargo package-metadata table and no
    /// version file. The document's own grammar is the tag on
    /// `ParticipantMetadata` itself, a format discriminator rather than a
    /// negotiated identity, so it needs no entry here.
    pub mod compatibility {
        use crate::version::FrameworkVersion;

        /// The canonical spelling of the framework train this binary was built
        /// from. The const-eval metadata writer needs a string; the value it
        /// spells is `FrameworkVersion::CURRENT`.
        pub const FRAMEWORK: &str = FrameworkVersion::CURRENT_SPELLING;

        pub use crate::participant_metadata_json;
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
    pub use crate::participant::metadata::ParticipantKind;

    /// The connection vocabulary `#[phoxal::driver(connection = …)]` declares
    /// from, and the const-eval writer that spells the declared kind into the
    /// embedded metadata document.
    pub use crate::model::connection::{ConnectionKind, ConnectionPayload};
    pub use crate::participant::metadata::connection_json;

    /// The cadence `#[phoxal::step(hz = …)]` returns from
    /// `Participant::__step_schedule`.
    pub use crate::participant::scheduler::StepSchedule;
}
