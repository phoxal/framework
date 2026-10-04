//! Typed Runtime authoring, scheduling, bounded inputs and outputs, and process execution.
//!
//! # Authoring a runtime
//!
//! Author a runtime as one inherent impl on your own struct and attach its
//! endpoint contract by type; the `#[phoxal::runtime]` attribute lowers
//! the block onto the engine the process runner drives. This recipe is
//! the compiled spelling the integration tests use — messages with
//! protobuf tags, an endpoint contract, one `#[init]` constructor, one
//! `#[handle]` per operation, one optional `#[step]`, and one
//! `#[publish]` per state output:
//!
//! ```no_run
//! #[phoxal::messages(package = "example.counter.v1")]
//! mod counter {
//!     use phoxal::contracts::{RequestReply, State};
//!
//!     pub struct ResetRequest {
//!         #[phoxal(tag = 1)]
//!         pub request_id: u64,
//!     }
//!
//!     pub enum ResetResponse {
//!         #[phoxal(tag = 1)]
//!         Reset,
//!     }
//!
//!     pub struct CounterState {
//!         #[phoxal(tag = 1)]
//!         pub count: u64,
//!     }
//!
//!     /// The counter's endpoint contract.
//!     #[phoxal::endpoints]
//!     pub struct CounterApi {
//!         #[phoxal::operation]
//!         reset: RequestReply<ResetRequest, ResetResponse>,
//!
//!         #[phoxal::output]
//!         status: State<CounterState>,
//!     }
//! }
//!
//! use phoxal::Result;
//! use phoxal::runtime::Context;
//! use phoxal::runtime::behavior::{Sequence, Tree};
//! use schemars::JsonSchema;
//! use std::time::Duration;
//!
//! /// The launch document supplies one per admitted runtime; the derive
//! /// supplies the schema the launcher validates it against.
//! #[derive(Debug, serde::Deserialize, JsonSchema, phoxal::Config)]
//! struct CounterConfig {
//!     start: u64,
//! }
//!
//! struct Counter {
//!     count: u64,
//!     hold: Tree<Counter>,
//! }
//!
//! #[phoxal::runtime(contract = counter::CounterApi, period_ms = 20)]
//! impl Counter {
//!     #[init]
//!     fn new(config: CounterConfig) -> Result<Self> {
//!         Ok(Self {
//!             count: config.start,
//!             // A behavior tree is an ordinary stored value built once:
//!             // no scheduler of its own, ticked from the step below.
//!             hold: Sequence::<Counter>::new()
//!                 .delay(Duration::from_millis(100))
//!                 .into_node()
//!                 .build()?,
//!         })
//!     }
//!
//!     #[handle(reset)]
//!     fn reset(
//!         &mut self,
//!         _ctx: &mut Context<'_, Self>,
//!         _request: counter::ResetRequest,
//!     ) -> Result<counter::ResetResponse> {
//!         self.count = 0;
//!         Ok(counter::ResetResponse::Reset)
//!     }
//!
//!     #[step]
//!     fn advance(&mut self, ctx: &mut Context<'_, Self>) -> Result<()> {
//!         // The tree advances on the execution clock; its progress and
//!         // typed outcome reach observers through the accepted
//!         // diagnostics diary, never through the step's own outputs.
//!         self.hold.tick(ctx)
//!     }
//!
//!     #[publish(status)]
//!     fn status(&self) -> counter::CounterState {
//!         counter::CounterState { count: self.count }
//!     }
//! }
//!
//! fn main() -> Result<()> {
//!     phoxal::runtime::run::<Counter>()
//! }
//! ```
//!
//! One `#[init]` constructor validates and builds the state; one
//! `#[handle(endpoint)]` method serves each declared operation or queued
//! input; one optional `#[step]` runs every period; one `#[publish]`
//! projects each state output. Handlers on the same invocation merge by
//! their admitted command order across every operation endpoint, with
//! declaration order breaking exact ties. Behavior trees are ordinary
//! stored values ticked from a step — see [`crate::runtime::behavior`] — and their
//! accepted diagnostics flow through the runtime's bounded diary.
//!
//! The internal [`crate::runtime::Runtime`] trait and its runner plumbing stay private to
//! the engine: the adapters the attribute generates, the transport
//! runner are their only implementors.
//! Local in-process testing drives the same generated adapter through
//! [`crate::runtime::Harness`] with an explicit virtual clock; the
//! integration tests under `phoxal/tests/` are complete working examples
//! of that harness recipe.
//!
//! # Cargo
//!
//! ```toml
//! [dependencies]
//! phoxal = { version = "0.0.0-dev", default-features = false, features = ["runtime"] }
//! ```
//!
//! [`crate::runtime::RuntimeRunner`] owns validation, admission, scheduling, reset, and
//! shutdown for the process's admitted runtimes.

pub mod artifact;
pub mod authoring;
pub mod behavior;
pub mod capture;
pub mod connection;
mod context;
mod core;
pub mod execution_protocol;
mod harness;
pub mod input;
mod operation;
pub mod outputs;
mod pending;
pub mod runner;
mod schedule;
/// Generated-code transport adapters shared by runtime decoders, the
/// supervisor, and session clients.  Not a public transport handle.
pub mod transport;

// The collector attributes live in the runtime namespace so the generated
// provider glue (and the SDK's own machinery tests) can use the exact
// `#[phoxal::runtime::inputs]` and `#[phoxal::runtime::input(...)]`
// spellings.  A package's endpoint authority is its authored service
// declaration; these attributes are the expansion layer that declaration
// lowers to, not a hand-authoring path.
pub use phoxal_macros::outputs;
pub use phoxal_macros::{input, inputs};

pub use context::{Context, ContextResources, EndpointView, Leased, Observation};
pub use core::{
    AcceptedInvocation, Config, ConfigSchema, ConfigSchemaValue, ExecutionDuration, ExecutionTime,
    InitContext, Invocation, InvocationError, LaunchedRuntime, ObservationStamp, OutputAdmission,
    RegisteredRuntime, Runtime, RuntimeContract, RuntimeOwner, RuntimeSpec, RuntimeSpecError,
    RuntimeStatus, Sample, StepContext, initialize, invoke, run, run_registered,
};
pub use harness::{
    ConfigDocument, Harness, HarnessAttachment, HarnessCall, HarnessCommands, HarnessDriver,
    HarnessError, HarnessQueue, HarnessRequest, MAX_RELEASES_PER_ADVANCE, new_harness_driver,
};
pub use operation::{
    ManagedOperation, OperationCompletion, OperationError, OperationKey, OperationOutcome,
    OperationPolicy, OperationState,
};
pub use outputs::{CallTicket, Outputs};
pub use pending::{MAX_PENDING_CALLS, PendingCalls, PendingOwner, next_execution_epoch};
pub use runner::{
    InputSource, OutputSink, PollOutcome, RunnerError, RuntimeClock, RuntimeLaunch,
    RuntimeLaunchManifest, RuntimeRunner, SystemClock,
};
pub use schedule::{HardwareInvocation, HardwareSchedule, ScheduleError};

pub use input::{
    Activation, CallCompletion, Capacity, CapacityError, Command, CommandId, CommandOrder,
    CommandOrderError, Commands, Completions, Events, InputSet, InputSnapshot, Latest, Operation,
    OperationInputError, Read, ReadCompletion, ReadError, ReadStatus, ReadSuccess, Reply, Request,
    RequestCompletion, RequestError, Samples, Setpoint, Stream, StreamFailure, StreamItem,
    TransportInputSet,
};

/// Type checks shared by generated endpoint collectors.
pub mod macro_support {
    pub use crate::macro_support::*;
}
