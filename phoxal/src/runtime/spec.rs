//! The static metadata traits the macros target (D50/D59/D60).
//!
//! - [`RuntimeFields`] is emitted by `#[derive(phoxal::Runtime)]`: the id, the one
//!   API version, the config type, and the field-side contracts.
//! - [`RuntimeBehavior`] is emitted by `#[phoxal::runtime]`: the server-side
//!   contracts plus the lifecycle dispatch (`__setup`/`__step`/`__shutdown`) the
//!   runner drives.
//! - [`Declares`] is a per-runtime marker (also emitted by the derive) that makes
//!   `SetupContext` builders reject undeclared contract families at compile time
//!   (D44).
//!
//! `REQUIRED_CONTRACTS` is the union of the field-side and server-side contracts;
//! `emit-apis` builds it at runtime, so no const slice concatenation is needed.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Serialize;

use crate::api::ApiVersion;
use crate::runtime::context::{SetupContext, ShutdownContext, StepContext};
use crate::runtime::server::ServerOutcome;

/// The direction a runtime uses a contract on a topic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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

/// One `{api_version, family, topic, direction}` contract a runtime participates
/// in. Built by the macros from a body type's [`ContractBody`](crate::api::ContractBody)
/// consts so the family/topic/version are single-sourced from the api tree.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ContractUse {
    /// The API version of the body (`<Body::Api as ApiVersion>::ID`).
    pub api_version: &'static str,
    /// The contract family id (`<Body as ContractBody>::FAMILY`).
    pub family: &'static str,
    /// The versionless topic key (`<Body as ContractBody>::TOPIC`).
    pub topic: &'static str,
    /// How the runtime uses the contract.
    pub direction: Direction,
}

/// Static metadata derived from a runtime struct (`#[derive(phoxal::Runtime)]`).
pub trait RuntimeFields: Sized + Send + 'static {
    /// The runtime id (`#[phoxal(id = "…")]`, default kebab of the type name).
    const ID: &'static str;
    /// The one API version this runtime — and the whole graph — runs against.
    type Api: ApiVersion;
    /// `<Self::Api as ApiVersion>::ID`, for metadata.
    const API_VERSION: &'static str;
    /// The runtime's typed config (`()` for official runtimes that read the
    /// robot model directly).
    type Config: serde::de::DeserializeOwned + JsonSchema + Send + 'static;
    /// The contracts derived from the struct's handle fields.
    const FIELD_CONTRACTS: &'static [ContractUse];
}

/// Lifecycle dispatch + server-side metadata (`#[phoxal::runtime]`).
///
/// The async methods are awaited by the runner in its own task (no `Send` bound
/// is required because the runtime future is not spawned).
#[allow(async_fn_in_trait)]
pub trait RuntimeBehavior: RuntimeFields {
    /// Contracts derived from `#[server]`/`#[server_snapshot]` signatures (empty
    /// until the query slice).
    const SERVER_CONTRACTS: &'static [ContractUse];

    /// The committed-snapshot state type (`()` when there is no `#[snapshot]`).
    type Snapshot: Send + Sync + 'static;

    /// Whether the runtime provides a committed snapshot (`#[snapshot]`).
    const HAS_SNAPSHOT: bool;

    /// The versionless topic keys of exclusive `#[server]` handlers.
    fn __exclusive_server_topics() -> &'static [&'static str];

    /// The versionless topic keys of concurrent `#[server_snapshot]` handlers.
    fn __snapshot_server_topics() -> &'static [&'static str];

    /// The scheduled-step cadence, or `None` if the runtime has no `#[step]`.
    fn __step_schedule() -> Option<StepSchedule>;

    /// Construct the runtime (`#[setup]`).
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
    /// `api_version`/`family` (from bus metadata) are validated against the
    /// handler's request body before decode.
    async fn __serve_exclusive(
        &mut self,
        topic: &str,
        api_version: &str,
        family: &str,
        request: &[u8],
    ) -> ServerOutcome;

    /// Serve one concurrent `#[server_snapshot]` query against a committed
    /// snapshot. Returns a boxed `Send` future so the runner can spawn it.
    fn __serve_snapshot(
        snapshot: Arc<Self::Snapshot>,
        topic: String,
        api_version: String,
        family: String,
        request: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = ServerOutcome> + Send>>;
}

/// Per-runtime marker that this runtime declared a *publish* handle for body `B`
/// (D44). Emitted by the derive for each `Publisher<B>` field; the
/// `SetupContext::publisher` builder carries `where Self: DeclaresPublish<B>`, so
/// publishing a family/direction the struct never declared is a compile error
/// (and would otherwise be IO `emit-apis` never reports).
pub trait DeclaresPublish<B: ?Sized> {}

/// Per-runtime marker that this runtime declared a *subscribe* handle for body
/// `B` (`Subscriber<B>`/`Latest<B>` fields). See [`DeclaresPublish`].
pub trait DeclaresSubscribe<B: ?Sized> {}

/// Per-runtime marker that this runtime declared a *query* handle for
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
