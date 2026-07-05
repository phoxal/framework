//! The static metadata traits the macros target (D50/D59/D60).
//!
//! - [`ParticipantSpec`] is emitted by `#[derive(phoxal::Service)]`,
//!   `#[derive(phoxal::Driver)]`, `#[derive(phoxal::Tool)]`, or
//!   `#[derive(phoxal::Simulator)]`: the artifact kind/class, id, the one API
//!   version, the config type, and the field-side contracts.
//! - [`ParticipantBehavior`] is emitted by `#[phoxal::behavior]`: the server-side
//!   contracts plus the lifecycle dispatch (`__setup`/`__step`/`__shutdown`) the
//!   runner drives.
//! - [`DeclaresPublish`]/[`DeclaresSubscribe`]/[`DeclaresQuery`] are per-participant
//!   markers (also emitted by the derive) that make `SetupContext` builders reject
//!   undeclared contract families/directions at compile time (D44).
//! - [`TypedGraphSurface`] is emitted by checked participant derives and gates
//!   `#[step]` / `#[server]` / `#[server_snapshot]` away from thin `Tool`
//!   runners.
//!
//! The full required-contract set is the union of [`ParticipantSpec::FIELD_CONTRACTS`]
//! and [`ParticipantBehavior::SERVER_CONTRACTS`]; `emit-apis` builds that union at
//! when `emit-apis` runs (see [`participant_metadata`](crate::participant::emit::participant_metadata)),
//! so there is no concatenated const slice.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Serialize;

use crate::bus::ApiVersion;
use crate::participant::context::{SetupContext, ShutdownContext, StepContext};
use crate::participant::server::ServerOutcome;

/// The direction a participant uses a contract on a topic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Publishes the body.
    Publish,
    /// Subscribes to the body (`Subscriber`/`Latest`).
    Subscribe,
    /// Sends a query request.
    QueryRequest,
    /// Receives a query response.
    QueryResponse,
    /// Serves a query request (server-side).
    ServerRequest,
    /// Emits a query response (server-side).
    ServerResponse,
}

/// One `{api_version, schema_id, family, topic, direction}` contract a participant
/// participates in. Built by the macros from a body type's
/// [`ContractBody`](crate::bus::ContractBody) consts so family/topic/schema/version
/// are single-sourced from the api tree.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ContractUse {
    /// The API version of the body (`<Body::Api as ApiVersion>::ID`).
    pub api_version: &'static str,
    /// The transitive wire-shape id (`<Body as ContractBody>::SCHEMA_ID`).
    pub schema_id: &'static str,
    /// The contract family id (`<Body as ContractBody>::FAMILY`).
    pub family: &'static str,
    /// The versionless topic key (`<Body as ContractBody>::TOPIC`).
    pub topic: &'static str,
    /// How the participant uses the contract.
    pub direction: Direction,
}

/// Static metadata derived from a participant struct.
pub trait ParticipantSpec: Sized + Send + 'static {
    /// The authoring kind that produced this artifact.
    const KIND: &'static str;
    /// Whether normal graph topology applies to this participant.
    const PARTICIPANT_CLASS: &'static str;
    /// The participant id (`#[phoxal(id = "…")]`, default kebab of the type name).
    const ID: &'static str;
    /// The one API version this participant runs against. The graph may mix
    /// generations: compatibility is per-contract `schema_id` agreement (#16).
    type Api: ApiVersion;
    /// `<Self::Api as ApiVersion>::ID`, for metadata.
    const API_VERSION: &'static str;
    /// The participant's typed config (`()` for official participants that read the
    /// robot model directly).
    type Config: serde::de::DeserializeOwned + JsonSchema + Send + 'static;
    /// The contracts derived from the struct's handle fields.
    const FIELD_CONTRACTS: &'static [ContractUse];
}

/// Marker emitted only by checked participant derives that expose the typed graph
/// surface (`#[step]` / `#[server]` / `#[server_snapshot]`).
#[diagnostic::on_unimplemented(
    message = "`{Self}` is a `Tool`, which is a thin raw-bus runner and has no typed-graph surface",
    label = "`#[step]` / `#[server]` is not allowed here; use the raw bus (`phoxal::raw`) instead"
)]
pub trait TypedGraphSurface {}

/// Marker emitted only by `#[derive(phoxal::Driver)]`.
#[doc(hidden)]
pub trait IsDriver {}

/// Marker emitted only by `#[derive(phoxal::Tool)]`.
#[doc(hidden)]
pub trait IsTool {}

/// Marker emitted only by `#[derive(phoxal::Simulator)]`.
#[doc(hidden)]
pub trait IsSimulator {}

/// Lifecycle dispatch + server-side metadata (`#[phoxal::behavior]`).
///
/// The async methods are awaited by the runner in its own task (no `Send` bound
/// is required because the participant future is not spawned).
#[allow(async_fn_in_trait)]
pub trait ParticipantBehavior: ParticipantSpec {
    /// Contracts derived from `#[server]`/`#[server_snapshot]` signatures (empty
    /// until the query slice).
    const SERVER_CONTRACTS: &'static [ContractUse];

    /// The committed-snapshot state type (`()` when there is no `#[snapshot]`).
    type Snapshot: Send + Sync + 'static;

    /// Whether the participant provides a committed snapshot (`#[snapshot]`).
    const HAS_SNAPSHOT: bool;

    /// The versionless topic keys of exclusive `#[server]` handlers.
    fn __exclusive_server_topics() -> &'static [&'static str];

    /// The versionless topic keys of concurrent `#[server_snapshot]` handlers.
    fn __snapshot_server_topics() -> &'static [&'static str];

    /// Validate explicit server topic expressions before startup declares
    /// queryables. The macro rejects an explicit key that differs from the
    /// request body's canonical topic and rejects duplicate server topics.
    fn __validate_server_topics() -> std::result::Result<(), String>;

    /// The scheduled-step cadence, or `None` if the participant has no `#[step]`.
    fn __step_schedule() -> Option<StepSchedule>;

    /// Construct the participant (`#[setup]`).
    async fn __setup(ctx: &mut SetupContext<Self>, config: Self::Config) -> crate::Result<Self>;

    /// Run one scheduled step (`#[step]`; a no-op when none is declared).
    async fn __step(&mut self, step: StepContext) -> crate::Result<()>;

    /// Graceful shutdown (`#[shutdown]`; a no-op when none is declared).
    async fn __shutdown(&mut self, ctx: ShutdownContext) -> crate::Result<()>;

    /// Commit the current state as a snapshot (calls the `#[snapshot]` provider;
    /// returns `()` when there is none).
    fn __take_snapshot(&self) -> Self::Snapshot;

    /// Serve one exclusive `#[server]` query (holds `&mut self`, serialized with
    /// `#[step]`). Awaited on the runner's main task. The request's
    /// `schema_id`/`family` (from bus metadata) are validated against the
    /// handler's request body before decode.
    async fn __serve_exclusive(
        &mut self,
        topic: &str,
        api_version: &str,
        schema_id: &str,
        family: &str,
        request: &[u8],
    ) -> ServerOutcome;

    /// Serve one concurrent `#[server_snapshot]` query against a committed
    /// snapshot. Returns a boxed `Send` future so the runner can spawn it.
    fn __serve_snapshot(
        snapshot: Arc<Self::Snapshot>,
        topic: String,
        api_version: String,
        schema_id: String,
        family: String,
        request: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = ServerOutcome> + Send>>;
}

/// Per-participant marker that this participant declared a *publish* handle for body `B`
/// (D44). Emitted by the derive for each `Publisher<B>` field; the
/// `SetupContext::publisher` builder carries `where Self: DeclaresPublish<B>`, so
/// publishing a family/direction the struct never declared is a compile error
/// (and would otherwise be IO `emit-apis` never reports).
pub trait DeclaresPublish<B: ?Sized> {}

/// Per-participant marker that this participant declared a *subscribe* handle for body
/// `B` (`Subscriber<B>`/`Latest<B>` fields). See [`DeclaresPublish`].
pub trait DeclaresSubscribe<B: ?Sized> {}

/// Per-participant marker that this participant declared a *query* handle for
/// `Req`/`Resp` (`Querier<Req, Resp>` fields). See [`DeclaresPublish`].
pub trait DeclaresQuery<Req: ?Sized, Resp: ?Sized> {}

/// The cadence + missed-tick policy of a `#[step(hz = …)]` loop (D34).
#[derive(Clone, Copy, Debug)]
pub struct StepSchedule {
    /// Target frequency in Hz.
    pub hz: f64,
    /// What to do after an overrun.
    pub missed_tick: MissedTick,
}

impl StepSchedule {
    /// A schedule at `hz` with the default (`Collapse`) missed-tick policy.
    pub const fn hz(hz: f64) -> Self {
        StepSchedule {
            hz,
            missed_tick: MissedTick::Collapse,
        }
    }

    /// The nominal step period.
    pub fn period(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.hz)
    }
}

/// Missed-tick policy after a step overruns its period (D34).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissedTick {
    /// Run a single step after an overrun and record `missed_ticks`; no burst
    /// catch-up. The v1 default.
    Collapse,
    /// Replay every missed tick. Reserved for offline replay.
    CatchUp,
}
