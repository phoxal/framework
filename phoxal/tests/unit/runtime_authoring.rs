//! Authored-runtime fixtures and acceptance tests for the inherent
//! `#[phoxal::runtime]` attachment: ordinary fields own state, handlers and
//! steps receive the SDK context, and state publications bind through
//! `#[publish]`.
//!
//! The fixtures live beside these tests because they exercise the public
//! authoring surface end to end through the real runtime owner and output
//! bindings; no transport is started here.

use phoxal::Result;
use phoxal::runtime::Context;

/// Explicit acceptance boundary for low-level generated-binding probes.
/// These tests supply candidate states directly to inspect tree internals;
/// both transient and projected products are encoded before committing.
fn accept_candidate<R>(
    runtime: &R,
    context: &phoxal::runtime::StepContext,
    state: R::State,
    inputs: &R::Inputs,
) -> phoxal::Result<(R::State, R::Outputs)>
where
    R: phoxal::runtime::RegisteredRuntime,
    R::Inputs: phoxal::runtime::input::InputSet,
{
    use phoxal::runtime::input::InputSet;
    use phoxal::runtime::outputs::{OutputBindings, OutputSet};
    let resolve = |field: &str| {
        R::Inputs::FIELDS
            .iter()
            .find(|input| input.name == field)
            .and_then(|input| input.port_signature)
    };
    let result =
        phoxal::runtime::invoke(runtime, context, state, inputs).and_then(|(state, outputs)| {
            outputs.encode_transport(*context, &resolve, "unit-owner")?;
            OutputBindings::encode_transport(runtime, &state, *context, &resolve, "unit-owner")?;
            Ok((state, outputs))
        });
    if result.is_ok() {
        runtime.accepted();
    } else {
        runtime.discarded();
    }
    result
}

// ---------------------------------------------------------------------------
// Countdown: the complete standalone provider from the runtime-authoring
// plan, with operations, a retained state publication, and an event output.
// ---------------------------------------------------------------------------

#[phoxal::messages(package = "phoxal.tests.authoring.countdown.v1")]
mod countdown {
    use phoxal::contracts::{Queue, RequestReply, State};

    pub struct StartRequest {
        #[phoxal(tag = 1)]
        pub job_id: u64,
        #[phoxal(tag = 2)]
        pub duration_ms: u64,
    }

    pub enum StartResponse {
        #[phoxal(tag = 1)]
        Accepted,
        #[phoxal(tag = 2)]
        Busy,
        #[phoxal(tag = 3)]
        Invalid,
    }

    pub struct CancelRequest {
        #[phoxal(tag = 1)]
        pub job_id: u64,
    }

    pub enum CancelResponse {
        #[phoxal(tag = 1)]
        Cancelled,
        #[phoxal(tag = 2)]
        UnknownJob,
    }

    pub struct CountdownState {
        #[phoxal(tag = 1)]
        pub active_job_id: Option<u64>,
        #[phoxal(tag = 2)]
        pub last_job_id: Option<u64>,
        #[phoxal(tag = 3)]
        pub last_outcome: Outcome,
    }

    pub enum Outcome {
        Unspecified = 0,
        Completed = 1,
        Cancelled = 2,
    }

    pub struct FinishedEvent {
        #[phoxal(tag = 1)]
        pub job_id: u64,
        #[phoxal(tag = 2)]
        pub outcome: Outcome,
    }

    /// The countdown service's endpoint contract.
    #[phoxal::endpoints]
    pub struct CountdownApi {
        #[phoxal::operation]
        start: RequestReply<StartRequest, StartResponse>,

        #[phoxal::operation]
        cancel: RequestReply<CancelRequest, CancelResponse>,

        #[phoxal::output]
        status: State<CountdownState>,

        #[phoxal::output(max_items = 16, max_bytes = 4096)]
        finished: Queue<FinishedEvent>,
    }
}

struct Job {
    id: u64,
    deadline_ns: u64,
}

pub(crate) struct Countdown {
    active: Option<Job>,
    last: Option<(u64, countdown::Outcome)>,
}

#[phoxal::runtime(contract = countdown::CountdownApi, period_ms = 20)]
impl Countdown {
    #[init]
    fn new(_config: ()) -> Result<Self> {
        Ok(Self {
            active: None,
            last: None,
        })
    }

    #[handle(start)]
    fn start(
        &mut self,
        ctx: &mut Context<'_, Self>,
        request: countdown::StartRequest,
    ) -> Result<countdown::StartResponse> {
        if request.job_id == 0 || !(1..=60_000).contains(&request.duration_ms) {
            return Ok(countdown::StartResponse::Invalid);
        }
        if self.active.is_some() {
            return Ok(countdown::StartResponse::Busy);
        }
        if self
            .last
            .as_ref()
            .is_some_and(|(id, _)| *id == request.job_id)
        {
            return Ok(countdown::StartResponse::Invalid);
        }
        let Some(deadline_ns) = ctx
            .now()
            .as_nanos()
            .checked_add(request.duration_ms * 1_000_000)
        else {
            return Ok(countdown::StartResponse::Invalid);
        };
        self.active = Some(Job {
            id: request.job_id,
            deadline_ns,
        });
        Ok(countdown::StartResponse::Accepted)
    }

    #[handle(cancel)]
    fn cancel(
        &mut self,
        ctx: &mut Context<'_, Self>,
        request: countdown::CancelRequest,
    ) -> Result<countdown::CancelResponse> {
        if self
            .active
            .as_ref()
            .is_some_and(|job| job.id == request.job_id)
        {
            self.finish(ctx, countdown::Outcome::Cancelled)?;
            Ok(countdown::CancelResponse::Cancelled)
        } else {
            Ok(countdown::CancelResponse::UnknownJob)
        }
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> Result<()> {
        if self
            .active
            .as_ref()
            .is_some_and(|job| ctx.now().as_nanos() >= job.deadline_ns)
        {
            self.finish(ctx, countdown::Outcome::Completed)?;
        }
        Ok(())
    }

    #[publish(status)]
    fn status(&self) -> countdown::CountdownState {
        countdown::CountdownState {
            active_job_id: self.active.as_ref().map(|job| job.id),
            last_job_id: self.last.as_ref().map(|(id, _)| *id),
            last_outcome: self
                .last
                .as_ref()
                .map_or(countdown::Outcome::Unspecified, |(_, outcome)| *outcome),
        }
    }

    fn finish(&mut self, ctx: &mut Context<'_, Self>, outcome: countdown::Outcome) -> Result<()> {
        if let Some(job) = self.active.take() {
            ctx.emit_finished(countdown::FinishedEvent {
                job_id: job.id,
                outcome,
            })?;
            self.last = Some((job.id, outcome));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Echo: a handler-only participant with no periodic step.
// ---------------------------------------------------------------------------

#[phoxal::messages(package = "phoxal.tests.authoring.echo.v1")]
mod echo {
    use phoxal::contracts::RequestReply;

    pub struct PokeRequest {
        #[phoxal(tag = 1)]
        pub payload: String,
    }

    pub struct PokeResponse {
        #[phoxal(tag = 1)]
        pub heard: String,
    }

    /// The echo service's endpoint contract.
    #[phoxal::endpoints]
    pub struct EchoApi {
        #[phoxal::operation]
        poke: RequestReply<PokeRequest, PokeResponse>,
    }
}

/// A publicly declared authored runtime: the launch attachment must not
/// require private visibility in either direction.
pub struct Echo {
    heard: Vec<String>,
}

#[phoxal::runtime(contract = echo::EchoApi, period_ms = 50)]
impl Echo {
    #[init]
    fn new(_config: ()) -> Result<Self> {
        Ok(Self { heard: Vec::new() })
    }

    #[handle(poke)]
    fn poke(
        &mut self,
        _ctx: &mut Context<'_, Self>,
        request: echo::PokeRequest,
    ) -> Result<echo::PokeResponse> {
        self.heard.push(request.payload.clone());
        Ok(echo::PokeResponse {
            heard: request.payload,
        })
    }
}

// ---------------------------------------------------------------------------
// Monitor: a bounded queue input beside a fresh-governed latest input, for
// the queue-capacity-one versus latest-retention regression.
// ---------------------------------------------------------------------------

#[phoxal::messages(package = "phoxal.tests.authoring.monitor.v1")]
mod monitor {
    use phoxal::contracts::{Latest, Queue, State};

    pub struct Trigger {
        #[phoxal(tag = 1)]
        pub sequence: u64,
    }

    pub struct Reading {
        #[phoxal(tag = 1)]
        pub level: u64,
    }

    pub struct BeatEvent {
        #[phoxal(tag = 1)]
        pub at_ns: u64,
    }

    pub struct Caption {
        #[phoxal(tag = 1)]
        pub label: String,
    }

    pub struct MonitorState {
        #[phoxal(tag = 1)]
        pub consumed: u64,
        #[phoxal(tag = 2)]
        pub last_fresh_level: Option<u64>,
        #[phoxal(tag = 3)]
        pub last_valid_grant: Option<u64>,
        #[phoxal(tag = 4)]
        pub last_capture_ns: Option<u64>,
        #[phoxal(tag = 5)]
        pub last_source: Option<String>,
        #[phoxal(tag = 6)]
        pub observed_caption: Option<String>,
        #[phoxal(tag = 7)]
        pub last_level_revision: Option<u64>,
    }

    /// The monitor's endpoint contract.
    #[phoxal::endpoints]
    pub struct MonitorApi {
        #[phoxal::input(max_items = 1, max_bytes = 256)]
        triggers: Queue<Trigger>,

        #[phoxal::input(max_items = 64, max_bytes = 64)]
        bursts: Queue<Trigger>,

        #[phoxal::input(max_age_ms = 100, max_bytes = 256)]
        level: Latest<Reading>,

        #[phoxal::input(max_age_ms = 1_000, max_bytes = 16)]
        caption: Latest<Caption>,

        #[phoxal::input(lease_ms = 100, max_bytes = 256)]
        grant: Latest<Reading>,

        #[phoxal::output]
        seen: State<MonitorState>,

        #[phoxal::output(max_items = 4, max_bytes = 256)]
        beats: Queue<BeatEvent>,

        #[phoxal::output(max_bytes = 256)]
        derived: Latest<Reading>,
    }
}

pub(crate) struct Monitor {
    consumed: u64,
    last_fresh_level: Option<u64>,
    last_valid_grant: Option<u64>,
    last_capture_ns: Option<u64>,
    last_source: Option<String>,
    observed_caption: Option<String>,
    last_level_revision: Option<u64>,
}

#[phoxal::runtime(contract = monitor::MonitorApi, period_ms = 20)]
impl Monitor {
    #[init]
    fn new(_config: ()) -> Result<Self> {
        Ok(Self {
            consumed: 0,
            last_fresh_level: None,
            last_valid_grant: None,
            last_capture_ns: None,
            last_source: None,
            observed_caption: None,
            last_level_revision: None,
        })
    }

    #[handle(triggers)]
    fn on_trigger(&mut self, _ctx: &mut Context<'_, Self>, item: monitor::Trigger) -> Result<()> {
        self.consumed = self.consumed.saturating_add(1);
        let _sequence = item.sequence;
        Ok(())
    }

    #[handle(bursts)]
    fn on_burst(&mut self, _ctx: &mut Context<'_, Self>, _item: monitor::Trigger) -> Result<()> {
        Ok(())
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> Result<()> {
        // Bind both frozen reads first, stage outputs through the same
        // context, and only then inspect the bound reads: the frozen input
        // cut outlives the mutable staging call without cloning payloads.
        let now = ctx.now();
        let level = ctx.level().fresh();
        let grant = ctx.grant().valid();
        let provenance = ctx.level().sample().map(|sample| {
            (
                sample.stamp().capture_time().as_nanos(),
                sample.stamp().source().to_owned(),
                sample.stamp().revision(),
            )
        });
        let observed_caption = ctx
            .caption()
            .sample()
            .map(|sample| sample.payload().label.clone());
        ctx.emit_beats(monitor::BeatEvent {
            at_ns: now.as_nanos(),
        })?;
        // A fresh observation publishes a derived latest value; a stale or
        // absent one publishes nothing, leaving the previously accepted
        // retained record in place.
        if let Some(reading) = level {
            ctx.publish_derived(monitor::Reading {
                level: reading.level,
            })?;
        }
        self.last_fresh_level = level.map(|reading| reading.level);
        self.last_valid_grant = grant.map(|reading| reading.level);
        self.last_capture_ns = provenance.as_ref().map(|(ns, _, _)| *ns);
        self.last_source = provenance.as_ref().map(|(_, source, _)| source.clone());
        self.last_level_revision = provenance.as_ref().and_then(|(_, _, revision)| *revision);
        self.observed_caption = observed_caption;
        Ok(())
    }

    #[publish(seen)]
    fn project(&self) -> monitor::MonitorState {
        monitor::MonitorState {
            consumed: self.consumed,
            last_fresh_level: self.last_fresh_level,
            last_valid_grant: self.last_valid_grant,
            last_capture_ns: self.last_capture_ns,
            last_source: self.last_source.clone(),
            observed_caption: self.observed_caption.clone(),
            last_level_revision: self.last_level_revision,
        }
    }
}

// ---------------------------------------------------------------------------
// Ticker: a step-only runtime with no operations and no queued inputs, the
// authoring shape of a periodic sensor or controller.
// ---------------------------------------------------------------------------

#[phoxal::messages(package = "phoxal.tests.authoring.ticker.v1")]
mod ticker {
    use phoxal::contracts::State;

    pub struct TickState {
        #[phoxal(tag = 1)]
        pub count: u64,
    }

    /// The ticker's endpoint contract.
    #[phoxal::endpoints]
    pub struct TickerApi {
        #[phoxal::output]
        ticks: State<TickState>,
    }
}

pub(crate) struct Ticker {
    count: u64,
}

#[phoxal::runtime(contract = ticker::TickerApi, period_ms = 20)]
impl Ticker {
    #[init]
    fn new(_config: ()) -> Result<Self> {
        Ok(Self { count: 0 })
    }

    #[step]
    fn advance(&mut self, _ctx: &mut Context<'_, Self>) -> Result<()> {
        self.count = self.count.saturating_add(1);
        Ok(())
    }

    #[publish(ticks)]
    fn ticks(&self) -> ticker::TickState {
        ticker::TickState { count: self.count }
    }
}

// ---------------------------------------------------------------------------
// Gate: a configured runtime whose initializer takes a locally named
// configuration type. The generated adapter consumes that type one module
// below this scope, so the authoring spelling must resolve there too.
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, phoxal::Config)]
pub(crate) struct GateSettings {
    #[serde(default = "default_threshold")]
    threshold: u64,
}

fn default_threshold() -> u64 {
    8
}

#[phoxal::messages(package = "phoxal.tests.authoring.gate.v1")]
mod gate {
    use phoxal::contracts::State;

    pub struct GateState {
        #[phoxal(tag = 1)]
        pub threshold: u64,
    }

    /// The gate service's endpoint contract.
    #[phoxal::endpoints]
    pub struct GateApi {
        #[phoxal::output]
        status: State<GateState>,
    }
}

pub(crate) struct Gate {
    threshold: u64,
}

#[phoxal::runtime(contract = gate::GateApi, period_ms = 20)]
impl Gate {
    #[init]
    fn new(config: GateSettings) -> Result<Self> {
        Ok(Self {
            threshold: config.threshold,
        })
    }

    #[publish(status)]
    fn status(&self) -> gate::GateState {
        gate::GateState {
            threshold: self.threshold,
        }
    }
}

// The same contract attached through the remaining supported configuration
// spellings: nested, `self`-qualified, and crate-rooted paths all resolve
// from the generated adapter one module below this scope.
mod gate_config {
    #[derive(serde::Deserialize, phoxal::Config)]
    pub struct Settings {
        #[serde(default = "default_threshold")]
        pub threshold: u64,
    }

    fn default_threshold() -> u64 {
        8
    }
}

pub(crate) struct NestedGate {
    threshold: u64,
}

#[phoxal::runtime(contract = gate::GateApi, period_ms = 20)]
impl NestedGate {
    #[init]
    fn new(config: gate_config::Settings) -> Result<Self> {
        Ok(Self {
            threshold: config.threshold,
        })
    }

    #[publish(status)]
    fn status(&self) -> gate::GateState {
        gate::GateState {
            threshold: self.threshold,
        }
    }
}

pub(crate) struct SelfGate {
    threshold: u64,
}

#[phoxal::runtime(contract = gate::GateApi, period_ms = 20)]
impl SelfGate {
    #[init]
    fn new(config: self::gate_config::Settings) -> Result<Self> {
        Ok(Self {
            threshold: config.threshold,
        })
    }

    #[publish(status)]
    fn status(&self) -> gate::GateState {
        gate::GateState {
            threshold: self.threshold,
        }
    }
}

pub(crate) struct RootedGate {
    threshold: u64,
}

#[phoxal::runtime(contract = gate::GateApi, period_ms = 20)]
impl RootedGate {
    #[init]
    fn new(config: self::gate_config::Settings) -> Result<Self> {
        Ok(Self {
            threshold: config.threshold,
        })
    }

    #[publish(status)]
    fn status(&self) -> gate::GateState {
        gate::GateState {
            threshold: self.threshold,
        }
    }
}

// ---------------------------------------------------------------------------
// Valve: a leased latest output beside retained state, the projected
// setpoint shape. Initialization must never grant it publication authority.
// ---------------------------------------------------------------------------

#[phoxal::messages(package = "phoxal.tests.authoring.valve.v1")]
mod valve {
    use phoxal::contracts::{Latest, State};

    pub struct FlowSetpoint {
        #[phoxal(tag = 1)]
        pub rate: u64,
    }

    pub struct ValveState {
        #[phoxal(tag = 1)]
        pub authorized_rate: Option<u64>,
    }

    /// The valve service's endpoint contract.
    #[phoxal::endpoints]
    pub struct ValveApi {
        #[phoxal::output(projection = state, lease_ms = 100, max_bytes = 256)]
        target: Latest<FlowSetpoint>,

        #[phoxal::output]
        status: State<ValveState>,
    }
}

pub(crate) struct Valve {
    authorized_rate: Option<u64>,
}

#[phoxal::runtime(contract = valve::ValveApi, period_ms = 20)]
impl Valve {
    #[init]
    fn new(_config: ()) -> Result<Self> {
        Ok(Self {
            authorized_rate: None,
        })
    }

    #[step]
    fn advance(&mut self, _ctx: &mut Context<'_, Self>) -> Result<()> {
        // A real driver would derive authorization from leased inputs;
        // this fixture keeps the projection withdrawn throughout.
        Ok(())
    }

    #[publish(target)]
    fn target(&self) -> Option<valve::FlowSetpoint> {
        self.authorized_rate
            .map(|rate| valve::FlowSetpoint { rate })
    }

    #[publish(status)]
    fn status(&self) -> valve::ValveState {
        valve::ValveState {
            authorized_rate: self.authorized_rate,
        }
    }
}

// ---------------------------------------------------------------------------
// Split: two queued outputs with different capacities, for whole-candidate
// retention across endpoints.
// ---------------------------------------------------------------------------

#[phoxal::messages(package = "phoxal.tests.authoring.split.v1")]
mod split {
    use phoxal::contracts::{Queue, RequestReply, State};

    pub struct PulseRequest {
        #[phoxal(tag = 1)]
        pub count: u32,
    }

    pub enum PulseResponse {
        #[phoxal(tag = 1)]
        Pulsed,
    }

    pub struct PulseEvent {
        #[phoxal(tag = 1)]
        pub index: u64,
    }

    pub struct SplitState {
        #[phoxal(tag = 1)]
        pub pulses: u64,
    }

    /// The split fixture's endpoint contract.
    #[phoxal::endpoints]
    pub struct SplitApi {
        #[phoxal::operation]
        pulse: RequestReply<PulseRequest, PulseResponse>,

        #[phoxal::output(max_items = 4, max_bytes = 256)]
        alpha: Queue<PulseEvent>,

        #[phoxal::output(max_items = 1, max_bytes = 256)]
        beta: Queue<PulseEvent>,

        #[phoxal::output]
        status: State<SplitState>,
    }
}

pub(crate) struct Split;

#[phoxal::runtime(contract = split::SplitApi, period_ms = 20)]
impl Split {
    #[init]
    fn new(_config: ()) -> Result<Self> {
        Ok(Self)
    }

    #[handle(pulse)]
    fn pulse(
        &mut self,
        ctx: &mut Context<'_, Self>,
        request: split::PulseRequest,
    ) -> Result<split::PulseResponse> {
        for index in 0..request.count {
            ctx.emit_alpha(split::PulseEvent {
                index: u64::from(index),
            })?;
            ctx.emit_beta(split::PulseEvent {
                index: u64::from(index),
            })?;
        }
        Ok(split::PulseResponse::Pulsed)
    }

    #[publish(status)]
    fn status(&self) -> split::SplitState {
        split::SplitState { pulses: 0 }
    }
}

// ---------------------------------------------------------------------------
// Replyer: a handler whose answer is valid while its State publication can
// be made oversized, so a failing projection must not expose the reply.
// ---------------------------------------------------------------------------

#[phoxal::messages(package = "phoxal.tests.authoring.replyer.v1")]
mod replyer {
    use phoxal::contracts::{RequestReply, State};

    pub struct AskRequest {
        #[phoxal(tag = 1)]
        pub padding: String,
    }

    pub struct AskResponse {
        #[phoxal(tag = 1)]
        pub value: u64,
    }

    pub struct ReplyerState {
        #[phoxal(tag = 1)]
        pub journal: String,
    }

    /// The replyer fixture's endpoint contract.
    #[phoxal::endpoints]
    pub struct ReplyerApi {
        #[phoxal::operation]
        ask: RequestReply<AskRequest, AskResponse>,

        #[phoxal::output(max_bytes = 64)]
        status: State<ReplyerState>,
    }
}

pub(crate) struct Replyer {
    journal: String,
}

#[phoxal::runtime(contract = replyer::ReplyerApi, period_ms = 20)]
impl Replyer {
    #[init]
    fn new(_config: ()) -> Result<Self> {
        Ok(Self {
            journal: String::new(),
        })
    }

    #[handle(ask)]
    fn ask(
        &mut self,
        _ctx: &mut Context<'_, Self>,
        request: replyer::AskRequest,
    ) -> Result<replyer::AskResponse> {
        self.journal = request.padding;
        Ok(replyer::AskResponse { value: 42 })
    }

    #[publish(status)]
    fn status(&self) -> replyer::ReplyerState {
        replyer::ReplyerState {
            journal: self.journal.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Swells: a configuration-driven State payload, so a reset can fail its
// fresh bootstrap publication terminally.
// ---------------------------------------------------------------------------

#[phoxal::messages(package = "phoxal.tests.authoring.swells.v1")]
mod swells {
    use phoxal::contracts::State;

    pub struct SwellsState {
        #[phoxal(tag = 1)]
        pub payload: String,
    }

    /// The swells fixture's endpoint contract.
    #[phoxal::endpoints]
    pub struct SwellsApi {
        #[phoxal::output(max_bytes = 64)]
        status: State<SwellsState>,
    }
}

#[derive(serde::Deserialize, serde::Serialize, phoxal::Config)]
pub(crate) struct SwellsConfig {
    #[serde(default)]
    payload: String,
}

pub(crate) struct Swells {
    payload: String,
}

#[phoxal::runtime(contract = swells::SwellsApi, period_ms = 20)]
impl Swells {
    #[init]
    fn new(config: SwellsConfig) -> Result<Self> {
        Ok(Self {
            payload: config.payload,
        })
    }

    #[publish(status)]
    fn status(&self) -> swells::SwellsState {
        swells::SwellsState {
            payload: self.payload.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Flaky: whole-output admission and terminal failure evidence.
// ---------------------------------------------------------------------------

#[phoxal::messages(package = "phoxal.tests.authoring.flaky.v1")]
mod flaky {
    use phoxal::contracts::{Queue, RequestReply};

    pub struct TryRequest {
        #[phoxal(tag = 1)]
        pub explode: bool,
        #[phoxal(tag = 2)]
        pub sparks: u32,
    }

    pub enum TryResponse {
        #[phoxal(tag = 1)]
        Armed,
    }

    pub struct Spark {
        #[phoxal(tag = 1)]
        pub index: u64,
    }

    /// The flaky service's endpoint contract.
    #[phoxal::endpoints]
    pub struct FlakyApi {
        #[phoxal::operation(max_bytes = 64)]
        arm: RequestReply<TryRequest, TryResponse>,

        #[phoxal::output(max_items = 2, max_bytes = 256)]
        sparks: Queue<Spark>,
    }
}

pub(crate) struct Flaky {
    explode_next: bool,
}

#[phoxal::runtime(contract = flaky::FlakyApi, period_ms = 20)]
impl Flaky {
    #[init]
    fn new(_config: ()) -> Result<Self> {
        Ok(Self {
            explode_next: false,
        })
    }

    #[handle(arm)]
    fn arm(
        &mut self,
        ctx: &mut Context<'_, Self>,
        request: flaky::TryRequest,
    ) -> Result<flaky::TryResponse> {
        if self.explode_next {
            return Err(phoxal::anyhow!("the arm handler failed on purpose"));
        }
        self.explode_next = request.explode;
        // A staged batch that exceeds the declared capacity must be
        // rejected as a whole downstream, never partially published.
        for index in 0..request.sparks {
            ctx.emit_sparks(flaky::Spark {
                index: u64::from(index),
            })?;
        }
        Ok(flaky::TryResponse::Armed)
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

mod tests {
    use super::Countdown;
    use super::Echo;
    use super::Flaky;
    use super::Gate;
    use super::GateSettings;
    use super::Monitor;
    use super::Replyer;
    use super::Split;
    use super::Swells;
    use super::SwellsConfig;
    use super::Ticker;
    use super::countdown;
    use super::echo;
    use super::flaky;
    use super::gate_config;
    use super::monitor;
    use super::phoxal_runtime_countdown;
    use super::phoxal_runtime_echo;
    use super::phoxal_runtime_flaky;
    use super::phoxal_runtime_gate;
    use super::phoxal_runtime_monitor;
    use super::phoxal_runtime_nested_gate;
    use super::phoxal_runtime_rooted_gate;
    use super::phoxal_runtime_self_gate;
    use super::phoxal_runtime_ticker;
    use super::phoxal_runtime_valve;
    use super::replyer;
    use super::split;
    use super::ticker;
    use phoxal::runtime::input::{
        Capacity, Command, CommandId, CommandOrder, Commands, InputSet, InputSnapshot, Latest,
        Samples, Setpoint,
    };
    use phoxal::runtime::outputs::OutputBindings;
    use phoxal::runtime::outputs::{OutputKind, OutputSet};
    use phoxal::runtime::{
        ExecutionDuration, ExecutionTime, InvocationError, ObservationStamp, OutputAdmission,
        RegisteredRuntime, RuntimeOwner, RuntimeStatus, Sample, StepContext, initialize,
    };

    type CountdownOwner = RuntimeOwner<phoxal_runtime_countdown::Adapter>;
    type EchoOwner = RuntimeOwner<phoxal_runtime_echo::Adapter>;
    type FlakyOwner = RuntimeOwner<phoxal_runtime_flaky::Adapter>;

    const PERIOD_20MS: ExecutionDuration = ExecutionDuration::from_millis(20);

    fn at(millis: u64) -> ExecutionTime {
        ExecutionTime::from_nanos(millis * 1_000_000)
    }

    fn first_step(millis: u64) -> StepContext {
        StepContext::first(at(millis), PERIOD_20MS)
    }

    fn later_step(millis: u64, index: u64) -> StepContext {
        StepContext::from_previous(
            at(millis),
            PERIOD_20MS,
            Some(at(millis.saturating_sub(20))),
            0,
            index,
        )
    }

    fn ordered<Request, Response>(sequence: u64, request: Request) -> Command<Request, Response> {
        Command::with_order(CommandOrder::new(1, 0, CommandId::new(sequence)), request)
    }

    fn countdown_inputs(
        start: Vec<Command<countdown::StartRequest, countdown::StartResponse>>,
        cancel: Vec<Command<countdown::CancelRequest, countdown::CancelResponse>>,
    ) -> countdown::countdown_api::Inputs {
        let mut inputs = <countdown::countdown_api::Inputs as InputSnapshot>::empty();
        inputs.start = Commands::new(start);
        inputs.cancel = Commands::new(cancel);
        inputs
    }

    fn start(job_id: u64, duration_ms: u64) -> countdown::StartRequest {
        countdown::StartRequest {
            job_id,
            duration_ms,
        }
    }

    fn accept_countdown(
        owner: &mut CountdownOwner,
        millis: u64,
        index: u64,
        inputs: &countdown::countdown_api::Inputs,
    ) -> phoxal::Result<countdown::countdown_api::Outputs> {
        let context = if index == 0 {
            first_step(millis)
        } else {
            later_step(millis, index)
        };
        Ok(owner.accept(&context, inputs)?.into_outputs())
    }

    #[test]
    fn countdown_accepts_starts_and_finishes_in_execution_time() -> phoxal::Result<()> {
        let mut owner = CountdownOwner::new(phoxal_runtime_countdown::Adapter::new(), at(0), ())?;

        let outputs = accept_countdown(
            &mut owner,
            0,
            0,
            &countdown_inputs(vec![ordered(1, start(1, 3_000))], Vec::new()),
        )
        .unwrap();
        assert!(matches!(
            outputs.start_replies[0].response(),
            countdown::StartResponse::Accepted
        ));
        assert!(outputs.finished.is_empty());

        // Before the deadline nothing finishes.
        let outputs = accept_countdown(
            &mut owner,
            2_999,
            1,
            &countdown_inputs(Vec::new(), Vec::new()),
        )
        .unwrap();
        assert!(outputs.finished.is_empty());

        // At the execution-time deadline the job finishes exactly once.
        let outputs = accept_countdown(
            &mut owner,
            3_000,
            2,
            &countdown_inputs(Vec::new(), Vec::new()),
        )
        .unwrap();
        assert_eq!(outputs.finished.len(), 1);
        assert_eq!(outputs.finished[0].job_id, 1);
        assert!(matches!(
            outputs.finished[0].outcome,
            countdown::Outcome::Completed
        ));

        // The finished job's identifier cannot be reused immediately, and a
        // fresh job is no longer busy.
        let outputs = accept_countdown(
            &mut owner,
            3_020,
            3,
            &countdown_inputs(
                vec![ordered(1, start(1, 100)), ordered(2, start(2, 100))],
                Vec::new(),
            ),
        )
        .unwrap();
        assert!(matches!(
            outputs.start_replies[0].response(),
            countdown::StartResponse::Invalid
        ));
        assert!(matches!(
            outputs.start_replies[1].response(),
            countdown::StartResponse::Accepted
        ));
        Ok(())
    }

    #[test]
    fn countdown_rejects_invalid_and_busy_requests() -> phoxal::Result<()> {
        let mut owner = CountdownOwner::new(phoxal_runtime_countdown::Adapter::new(), at(0), ())?;

        let outputs = accept_countdown(
            &mut owner,
            0,
            0,
            &countdown_inputs(
                vec![
                    ordered(1, start(0, 100)),
                    ordered(2, start(5, 0)),
                    ordered(3, start(5, 60_001)),
                ],
                Vec::new(),
            ),
        )
        .unwrap();
        for reply in &outputs.start_replies {
            assert!(matches!(
                reply.response(),
                countdown::StartResponse::Invalid
            ));
        }

        let outputs = accept_countdown(
            &mut owner,
            20,
            1,
            &countdown_inputs(
                vec![ordered(1, start(5, 500)), ordered(2, start(6, 500))],
                Vec::new(),
            ),
        )
        .unwrap();
        assert!(matches!(
            outputs.start_replies[0].response(),
            countdown::StartResponse::Accepted
        ));
        assert!(matches!(
            outputs.start_replies[1].response(),
            countdown::StartResponse::Busy
        ));
        Ok(())
    }

    #[test]
    fn admitted_order_is_preserved_across_operation_endpoints() -> phoxal::Result<()> {
        // Cancel after start in admitted order: the cancel observes the job.
        let mut owner = CountdownOwner::new(phoxal_runtime_countdown::Adapter::new(), at(0), ())?;
        let outputs = accept_countdown(
            &mut owner,
            0,
            0,
            &countdown_inputs(
                vec![ordered(1, start(9, 60_000))],
                vec![ordered(2, countdown::CancelRequest { job_id: 9 })],
            ),
        )
        .unwrap();
        assert!(matches!(
            outputs.start_replies[0].response(),
            countdown::StartResponse::Accepted
        ));
        assert!(matches!(
            outputs.cancel_replies[0].response(),
            countdown::CancelResponse::Cancelled
        ));
        assert_eq!(outputs.finished.len(), 1);
        assert!(matches!(
            outputs.finished[0].outcome,
            countdown::Outcome::Cancelled
        ));

        // The same requests in reversed admitted order produce the reversed
        // domain outcome: the cancel runs first and finds no job.
        let mut owner = CountdownOwner::new(phoxal_runtime_countdown::Adapter::new(), at(0), ())?;
        let outputs = accept_countdown(
            &mut owner,
            0,
            0,
            &countdown_inputs(
                vec![ordered(2, start(9, 60_000))],
                vec![ordered(1, countdown::CancelRequest { job_id: 9 })],
            ),
        )
        .unwrap();
        assert!(matches!(
            outputs.cancel_replies[0].response(),
            countdown::CancelResponse::UnknownJob
        ));
        assert!(matches!(
            outputs.start_replies[0].response(),
            countdown::StartResponse::Accepted
        ));
        assert!(outputs.finished.is_empty());
        Ok(())
    }

    #[test]
    fn state_publishes_initially_and_after_each_accepted_invocation() -> phoxal::Result<()> {
        // This test proves the compiled binding surface only: the State<T>
        // endpoint semantic marks the state output for bootstrap, and the
        // binding encodes the projection for the initial and candidate
        // states. Supervisor bootstrap acceptance, reset republication, and
        // the absence of initial actuator authority are separate runtime
        // lifecycle paths this metadata evidence does not cover.
        let adapter = phoxal_runtime_countdown::Adapter::new();

        let status_field = <phoxal_runtime_countdown::Adapter as OutputBindings>::FIELDS
            .iter()
            .find(|field| field.name == "status")
            .expect("the status binding exists");
        assert_eq!(status_field.kind, OutputKind::State);
        assert!(
            status_field.bootstrap,
            "a State<T> output publishes initially without an authored bootstrap flag"
        );

        let state = initialize(&adapter, at(0), ())?;
        let status_publications = |state: &Countdown, context: StepContext| {
            <phoxal_runtime_countdown::Adapter as OutputBindings>::encode_transport(
                &adapter,
                state,
                context,
                &|_| None,
                "authoring-test",
            )
            .expect("state projections encode")
        };
        // The initial state publication is part of the State<T> endpoint
        // semantic: the bindings encode it before any ordinary work.
        let initial = status_publications(&state, first_step(0));
        assert_eq!(initial.len(), 1);

        // After an accepted invocation that starts a job, the same
        // publication surface evaluates the resulting candidate state.
        let inputs = countdown_inputs(vec![ordered(1, start(4, 1_000))], Vec::new());
        let (state, _outputs) = crate::runtime_authoring::accept_candidate(
            &adapter,
            &later_step(0, 1),
            state,
            &inputs,
        )?;
        let publications = status_publications(&state, later_step(20, 1));
        assert_eq!(publications.len(), 1);
        let signature = publications[0]
            .signature()
            .expect("the status port is signed");
        assert_eq!(signature.endpoint, "status");
        Ok(())
    }

    // -----------------------------------------------------------------
    // Explicit-time harness semantics over the same fixtures.
    // -----------------------------------------------------------------

    #[test]
    fn harness_publishes_bootstrap_state_before_any_release() -> phoxal::Result<()> {
        use phoxal::runtime::Harness;
        use std::time::Duration;

        let mut host = Harness::<Ticker>::new(())?;
        // The initial state publication is captured at construction,
        // before any release executes: the step counter is still zero.
        let initial = host.ticks().expect("bootstrap state is retained");
        assert_eq!(initial.count, 0);

        // Advancing to zero executes the first release exactly once; the
        // periodic publication reflects the stepped state.
        host.advance_to(Duration::ZERO)?;
        let stepped = host.ticks().expect("state publication is retained");
        assert_eq!(stepped.count, 1);

        // Repeated advancement to the same instant executes nothing.
        host.advance_to(Duration::ZERO)?;
        assert_eq!(
            host.ticks().expect("state publication is retained").count,
            1
        );
        Ok(())
    }

    #[test]
    fn harness_admits_queued_operations_in_stable_order() -> phoxal::Result<()> {
        use phoxal::runtime::Harness;
        use std::time::Duration;

        let mut host = Harness::<Echo>::new(())?;
        let first = host.enqueue_poke(echo::PokeRequest {
            payload: "first".to_owned(),
        })?;
        let second = host.enqueue_poke(echo::PokeRequest {
            payload: "second".to_owned(),
        })?;
        host.advance_to(Duration::ZERO)?;
        assert_eq!(
            host.reply(first)?.heard,
            "first",
            "replies correlate to their calls in admitted order"
        );
        assert_eq!(host.reply(second)?.heard, "second");

        // A reply is consumed exactly once.
        let mut drained = host;
        let replay = {
            let call = drained.enqueue_poke(echo::PokeRequest {
                payload: "third".to_owned(),
            })?;
            drained.advance_to(Duration::from_millis(50))?;
            drained.reply(call)?.heard.clone()
        };
        assert_eq!(replay, "third");
        let _ = &mut drained;
        Ok(())
    }

    #[test]
    fn harness_rejects_reversed_clocks_and_reports_remaining_work() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        let mut host = Harness::<Ticker>::new(())?;
        host.advance_to(Duration::from_secs(1))?;
        assert!(matches!(
            host.advance_to(Duration::from_millis(999)),
            Err(HarnessError::ClockReversed {
                requested: 999_000_000,
                current: 1_000_000_000
            })
        ));

        // One advance operation executes at most the documented release
        // budget: an hour at a 20 ms cadence reports exhaustion with the
        // outstanding count, preserving accepted progress.
        let mut long = Harness::<Ticker>::new(())?;
        let outcome = long.advance_to(Duration::from_secs(3_600));
        let HarnessError::WorkExhausted {
            executed,
            remaining,
        } = outcome.unwrap_err()
        else {
            panic!("an hour at a 20 ms cadence must exhaust the release budget")
        };
        assert_eq!(executed, phoxal::runtime::MAX_RELEASES_PER_ADVANCE);
        assert!(remaining > 0);
        // Progress is preserved: the step counter matches the executed
        // releases, and advancing again resumes the outstanding releases
        // (another full budget's worth before exhausting again).
        assert_eq!(
            long.ticks().expect("state publication is retained").count,
            executed
        );
        match long.advance_to(Duration::from_secs(3_600)) {
            Err(HarnessError::WorkExhausted {
                executed: resumed, ..
            }) => {
                assert_eq!(resumed, phoxal::runtime::MAX_RELEASES_PER_ADVANCE);
            }
            Err(other) => panic!("resumption must execute more releases: {other}"),
            Ok(_) => panic!("an hour at a 20 ms cadence cannot complete in two budgets"),
        }
        assert_eq!(
            long.ticks().expect("state publication is retained").count,
            executed * 2
        );
        Ok(())
    }

    #[test]
    fn harness_input_capacity_refusal_happens_before_invocation() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        // The monitor's trigger queue declares max_items = 1.
        let mut host = Harness::<Monitor>::new(())?;
        host.enqueue_triggers(monitor::Trigger { sequence: 1 })?;
        assert!(matches!(
            host.enqueue_triggers(monitor::Trigger { sequence: 2 }),
            Err(HarnessError::PendingFull {
                endpoint: "triggers"
            })
        ));
        // The refused item never reached the runtime: the consumed count
        // reflects exactly one admitted trigger.
        host.advance_to(Duration::ZERO)?;
        let seen = host.seen().expect("state publication is retained");
        assert_eq!(seen.consumed, 1);
        Ok(())
    }

    #[test]
    fn harness_admission_failure_is_terminal_and_publishes_nothing() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        // Stage a batch that exceeds the declared capacity of two: the
        // complete encoded batch fails admission.
        let mut host = Harness::<Flaky>::new(())?;
        let arm = host.enqueue_arm(flaky::TryRequest {
            explode: false,
            sparks: 3,
        })?;
        let failure = host.advance_to(Duration::ZERO).unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("batch exceeds item count bound: 3 > 2"),
            "the failure is the real capacity bound: {failure}"
        );
        // No part of the rejected batch was retained, the reply is gone
        // with the discarded invocation, and the execution is terminal.
        assert!(host.sparks().is_empty());
        // The correlation was issued and its candidate was rejected: no
        // reply exists for it and none will arrive.
        assert!(matches!(host.reply(arm), Err(HarnessError::ReplyConsumed)));
        assert!(matches!(
            host.advance_to(Duration::from_millis(20)),
            Err(HarnessError::Terminal { .. })
        ));
        Ok(())
    }

    #[test]
    fn harness_retained_effects_are_bounded_and_drainable() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        // The flaky contract retains at most 16 undrained sparks; driving
        // more than that without draining retires the harness execution
        // visibly instead of growing without bound.
        let mut host = Harness::<Flaky>::new(())?;
        let mut release = 0_u64;
        loop {
            let arm = host.enqueue_arm(flaky::TryRequest {
                explode: false,
                sparks: 2,
            })?;
            let outcome = host.advance_to(Duration::from_millis(release * 20));
            release += 1;
            if let Err(HarnessError::Terminal { .. }) = outcome {
                break;
            }
            outcome?;
            host.reply(arm)?;
            if release > 32 {
                panic!("retained bounds must engage within a bounded number of releases");
            }
        }
        assert!(host.sparks().len() <= 16);
        Ok(())
    }

    #[test]
    fn harness_pending_inputs_are_bounded_by_bytes_too() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        // The monitor's burst queue declares max_items = 64 but
        // max_bytes = 64: each trigger encodes to a few bytes, so the byte
        // bound must refuse long before the item bound.
        let mut host = Harness::<Monitor>::new(())?;
        let mut staged = 0_u64;
        loop {
            match host.enqueue_bursts(monitor::Trigger { sequence: 1 }) {
                Ok(()) => staged += 1,
                Err(HarnessError::PendingFull { endpoint: "bursts" }) => break,
                Err(other) => panic!("unexpected enqueue failure: {other}"),
            }
            if staged >= 64 {
                panic!("the byte bound must engage before the item bound");
            }
        }
        assert!(staged < 64, "the byte bound refused before 64 items");
        // The staged batch is admitted whole at the next release.
        host.advance_to(Duration::ZERO)?;
        let seen = host.seen().expect("state publication is retained");
        assert_eq!(seen.consumed, 0, "burst items are not trigger items");
        Ok(())
    }

    #[test]
    fn harness_retention_failure_captures_no_part_of_the_rejected_batch() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        // The flaky contract retains at most 16 sparks, two per accepted
        // batch. Driving past the bound must leave a whole number of
        // batches retained: the overflowing batch contributes either all
        // of its items or none, never a part.
        let mut host = Harness::<Flaky>::new(())?;
        let mut releases = 0_u64;
        loop {
            let arm = host.enqueue_arm(flaky::TryRequest {
                explode: false,
                sparks: 2,
            })?;
            releases += 1;
            match host.advance_to(Duration::from_millis(releases * 20)) {
                Ok(_) => {
                    host.reply(arm)?;
                }
                Err(HarnessError::Terminal { .. }) => break,
                Err(other) => panic!("unexpected advance failure: {other}"),
            }
            if releases > 32 {
                panic!("retained bounds must engage within a bounded number of releases");
            }
        }
        let retained = host.sparks().len();
        assert!(retained <= 16);
        assert_eq!(
            retained % 2,
            0,
            "retention is whole-batch: {retained} sparks from two-per-batch acceptances"
        );
        Ok(())
    }

    #[test]
    fn harness_calls_from_before_a_reset_never_alias_calls_after_it() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        let mut host = Harness::<Echo>::new(())?;
        let stale = host.enqueue_poke(echo::PokeRequest {
            payload: "before-reset".to_owned(),
        })?;
        host.advance_to(Duration::ZERO)?;
        assert_eq!(host.reply(stale.clone())?.heard, "before-reset");

        host.reset(())?;
        // A fresh call after the reset answers with its own payload; its
        // reply is still retained while the stale token is checked, so the
        // discriminator cannot pass by consumption ordering.
        let fresh = host.enqueue_poke(echo::PokeRequest {
            payload: "after-reset".to_owned(),
        })?;
        host.advance_to(Duration::from_millis(50))?;
        assert!(matches!(
            host.reply(stale),
            Err(HarnessError::ReplyConsumed)
        ));
        assert_eq!(host.reply(fresh)?.heard, "after-reset");
        Ok(())
    }

    // -----------------------------------------------------------------
    // Review-correction regressions: whole-candidate reservation, owner-
    // bound tokens, bounded replies, reset failure, timeline exhaustion.
    // -----------------------------------------------------------------

    #[test]
    fn a_later_endpoint_rejection_exposes_no_earlier_endpoint_events() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        // alpha holds four events, beta holds one. The first invocation's
        // candidate fits both; the second's fits alpha but not beta. The
        // second candidate must be rejected whole: alpha's retained count
        // stays at one, proving the complete effect was reserved before
        // the owner accepted anything.
        let mut host = Harness::<Split>::new(())?;
        let first = host.enqueue_pulse(split::PulseRequest { count: 1 })?;
        host.advance_to(Duration::ZERO)?;
        assert!(matches!(host.reply(first)?, split::PulseResponse::Pulsed));

        // The first candidate's events remain retained and undrained, so
        // the second candidate fits alpha (1 + 1 <= 4) but not beta
        // (1 + 1 > 1): it must be rejected whole, leaving exactly the
        // first candidate's effects retained.
        let second = host.enqueue_pulse(split::PulseRequest { count: 1 })?;
        let failure = host.advance_to(Duration::from_millis(20)).unwrap_err();
        assert!(
            failure.to_string().contains("retained"),
            "the rejection is the retention bound: {failure}"
        );
        assert_eq!(
            host.alpha().len(),
            1,
            "only the first candidate's alpha events remain retained"
        );
        assert_eq!(host.beta().len(), 1);
        assert!(matches!(
            host.reply(second),
            Err(HarnessError::ReplyConsumed)
        ));
        assert!(matches!(
            host.advance_to(Duration::from_millis(40)),
            Err(HarnessError::Terminal { .. })
        ));
        Ok(())
    }

    #[test]
    fn a_failing_state_publication_exposes_no_reply() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        // The handler answers 42, but padding the journal beyond the
        // state output's byte bound makes the candidate's projection
        // unencodable: the whole candidate is rejected, so the reply is
        // never exposed even though the handler produced it.
        let mut host = Harness::<Replyer>::new(())?;
        let padding = "x".repeat(128);
        let ask = host.enqueue_ask(replyer::AskRequest { padding })?;
        let failure = host.advance_to(Duration::ZERO).unwrap_err();
        assert!(
            failure.to_string().contains("exceeds") || failure.to_string().contains("bound"),
            "the failure is the state byte bound: {failure}"
        );
        assert!(matches!(host.reply(ask), Err(HarnessError::ReplyConsumed)));
        // The bootstrap state remains untouched by the rejected candidate.
        assert_eq!(
            host.status().expect("bootstrap state remains").journal,
            "",
            "the rejected candidate published no state"
        );
        Ok(())
    }

    #[test]
    fn a_foreign_harness_token_cannot_consume_another_harness_reply() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        let mut alpha = Harness::<Echo>::new(())?;
        let mut beta = Harness::<Echo>::new(())?;
        let alpha_call = alpha.enqueue_poke(echo::PokeRequest {
            payload: "alpha".to_owned(),
        })?;
        let beta_call = beta.enqueue_poke(echo::PokeRequest {
            payload: "beta".to_owned(),
        })?;
        alpha.advance_to(Duration::ZERO)?;
        beta.advance_to(Duration::ZERO)?;

        // Each harness's reply answers only its own token.
        assert!(matches!(
            beta.reply(alpha_call.clone()),
            Err(HarnessError::ForeignCall)
        ));
        assert!(matches!(
            alpha.reply(beta_call.clone()),
            Err(HarnessError::ForeignCall)
        ));
        assert_eq!(alpha.reply(alpha_call)?.heard, "alpha");
        assert_eq!(beta.reply(beta_call)?.heard, "beta");
        Ok(())
    }

    #[test]
    fn undrained_replies_are_bounded_and_drained_repeats_stay_bounded() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        // The echo contract's poke operation retains at most its declared
        // item bound of undrained replies: driving past it rejects the
        // whole candidate visibly instead of accumulating without bound.
        let mut host = Harness::<Echo>::new(())?;
        let mut accepted = 0_u64;
        loop {
            // Deliberately undrained: the replies accumulate.
            let _call = host.enqueue_poke(echo::PokeRequest {
                payload: "unbounded?".to_owned(),
            })?;
            let millis = accepted * 50;
            match host.advance_to(Duration::from_millis(millis)) {
                Ok(_) => accepted += 1,
                Err(HarnessError::Terminal { .. }) if accepted >= 1 => break,
                Err(other) => panic!("unexpected advance failure at {accepted}: {other}"),
            }
            if accepted > 64 {
                panic!("the reply retention bound must engage within 64 undrained replies");
            }
        }
        // The echo contract's poke operation declares the default item
        // bound of 16 undrained replies.
        assert_eq!(
            accepted, 16,
            "undrained replies are bounded by the declared item bound"
        );

        // Repeated drained calls never grow harness-owned storage: the
        // consumed bookkeeping is an issued-count, not per-id tombstones.
        let mut cycling = Harness::<Echo>::new(())?;
        for round in 0..128_u64 {
            let call = cycling.enqueue_poke(echo::PokeRequest {
                payload: "drain".to_owned(),
            })?;
            cycling.advance_to(Duration::from_millis(round * 50))?;
            assert_eq!(cycling.reply(call)?.heard, "drain");
        }
        Ok(())
    }

    #[test]
    fn a_failed_reset_publication_is_terminal_and_retires_prior_retention() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        // A small payload initializes and publishes fine.
        let mut host = Harness::<Swells>::new(SwellsConfig {
            payload: "small".to_owned(),
        })?;
        assert_eq!(host.status().expect("bootstrap state").payload, "small");

        // A reset whose fresh bootstrap publication exceeds the state
        // bound fails terminally: no later invocation or reservation, and
        // the previously retained state is untouched by the failed reset.
        let oversized = "y".repeat(128);
        assert!(host.reset(SwellsConfig { payload: oversized }).is_err());
        assert!(matches!(
            host.advance_to(Duration::ZERO),
            Err(HarnessError::Terminal { .. })
        ));
        Ok(())
    }

    #[test]
    fn timeline_exhaustion_never_retries_an_accepted_release_stale() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        // Advancing to the end of the timeline reports work exhaustion
        // long before the end at a 20 ms cadence.
        let mut host = Harness::<Ticker>::new(())?;
        assert!(matches!(
            host.advance_to(Duration::from_nanos(u64::MAX)),
            Err(HarnessError::WorkExhausted { .. })
        ));
        // Resetting at the current explicit instant restarts the local
        // schedule there; the timeline's last representable release
        // executes exactly once, and further advancement is refused as an
        // exhausted timeline — never a stale-index failure.
        host.reset(())?;
        // At the timeline's end the successor cursor is not representable:
        // advancement is refused as a clock overflow before any release
        // executes, and retrying is refused identically — the owner was
        // never accepted ahead of its cursor.
        assert!(matches!(
            host.advance_to(Duration::from_nanos(u64::MAX)),
            Err(HarnessError::ClockOverflow)
        ));
        assert!(matches!(
            host.advance_to(Duration::from_nanos(u64::MAX)),
            Err(HarnessError::ClockOverflow)
        ));
        Ok(())
    }

    #[test]
    fn draining_replies_releases_their_byte_budget() -> phoxal::Result<()> {
        use phoxal::runtime::Harness;
        use std::time::Duration;

        // The arm operation declares only 64 reply bytes. Hundreds of
        // drained nonempty responses exceed that cumulative limit, proving
        // that draining releases bytes instead of retaining lifetime usage.
        let mut host = Harness::<Flaky>::new(())?;
        for round in 0..512_u64 {
            let arm = host.enqueue_arm(flaky::TryRequest {
                explode: false,
                sparks: 0,
            })?;
            host.advance_to(Duration::from_millis(round * 20))?;
            assert!(
                matches!(host.reply(arm)?, flaky::TryResponse::Armed),
                "drained round {round} must answer"
            );
        }
        Ok(())
    }

    #[test]
    fn a_newly_enqueued_call_is_pending_before_its_first_release() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        let mut host = Harness::<Echo>::new(())?;
        let call = host.enqueue_poke(echo::PokeRequest {
            payload: "waiting".to_owned(),
        })?;
        // Before any release the staged correlation is pending, not
        // consumed: its candidate has not run yet.
        assert!(matches!(
            host.reply(call.clone()),
            Err(HarnessError::ReplyPending)
        ));
        host.advance_to(Duration::ZERO)?;
        assert_eq!(host.reply(call)?.heard, "waiting");
        Ok(())
    }

    // -----------------------------------------------------------------
    // Ordinary Latest observation injection and Latest output capture.
    // -----------------------------------------------------------------

    /// One stamped injection helper with explicit provenance.
    fn stamped_level(level: u64, millis: u64) -> phoxal::runtime::Sample<monitor::Reading> {
        phoxal::runtime::Sample::new(
            monitor::Reading { level },
            phoxal::runtime::ObservationStamp::new(
                "test-sensor",
                phoxal::runtime::ExecutionTime::from_nanos(millis * 1_000_000),
                None,
            ),
        )
    }

    #[test]
    fn an_injected_latest_observation_transitions_from_fresh_to_stale() -> phoxal::Result<()> {
        use phoxal::runtime::Harness;
        use std::time::Duration;

        let mut host = Harness::<Monitor>::new(())?;

        // Before any injection the observation is unavailable and nothing
        // is published; a Latest marker alone authorizes no initial
        // publication.
        host.advance_to(Duration::ZERO)?;
        let _ = host.beats();
        let idle = host.seen().expect("state publication is retained");
        assert_eq!(idle.last_fresh_level, None);
        assert_eq!(host.derived(), None, "no initial latest publication");

        // One stamped value enters the NEXT frozen cut and stays fresh
        // through the declared inclusive age boundary. The monitor emits
        // one beat per release, so each release is advanced individually
        // and its beat drained.
        host.inject_level(stamped_level(7, 0))?;
        for millis in [20_u64, 40, 60, 80, 100] {
            host.advance_to(Duration::from_millis(millis))?;
            let _ = host.beats();
        }
        let boundary = host.seen().expect("state publication is retained");
        assert_eq!(boundary.last_fresh_level, Some(7));
        assert_eq!(boundary.last_capture_ns, Some(0));
        assert_eq!(boundary.last_source.as_deref(), Some("test-sensor"));
        assert_eq!(host.derived().map(|r| r.level), Some(7));

        // Just beyond the boundary the same retained observation (same
        // capture stamp and source) is stale: no derived publication, and
        // the previously accepted retained record survives.
        host.advance_to(Duration::from_millis(120))?;
        let _ = host.beats();
        let stale = host.seen().expect("state publication is retained");
        assert_eq!(
            stale.last_fresh_level, None,
            "the aged observation is stale"
        );
        assert_eq!(
            stale.last_capture_ns,
            Some(0),
            "the retained capture stamp is preserved across cuts"
        );
        assert_eq!(stale.last_source.as_deref(), Some("test-sensor"));
        assert_eq!(
            host.derived().map(|r| r.level),
            Some(7),
            "no publication this invocation leaves the retained record"
        );
        Ok(())
    }

    #[test]
    fn a_second_injection_replaces_the_pending_value_before_the_cut() -> phoxal::Result<()> {
        use phoxal::runtime::Harness;
        use std::time::Duration;

        let mut host = Harness::<Monitor>::new(())?;
        host.advance_to(Duration::ZERO)?;

        // Two injections before the next release: the later one replaces
        // the pending value whole — Latest retention, not a queue.
        host.inject_level(stamped_level(1, 20))?;
        host.inject_level(stamped_level(2, 20))?;
        host.advance_to(Duration::from_millis(20))?;
        let replaced = host.seen().expect("state publication is retained");
        assert_eq!(replaced.last_fresh_level, Some(2));
        assert_eq!(host.derived().map(|r| r.level), Some(2));

        // A staged replacement after a release does not change the
        // already-frozen cut: the state from the previous release still
        // shows the earlier value until the next release admits it.
        host.inject_level(stamped_level(3, 40))?;
        let frozen = host.seen().expect("state publication is retained");
        assert_eq!(
            frozen.last_fresh_level,
            Some(2),
            "the frozen cut is unchanged by a staged replacement"
        );
        host.advance_to(Duration::from_millis(40))?;
        let admitted = host.seen().expect("state publication is retained");
        assert_eq!(admitted.last_fresh_level, Some(3));
        assert_eq!(host.derived().map(|r| r.level), Some(3));
        Ok(())
    }

    #[test]
    fn an_oversized_injection_is_refused_and_the_pending_value_survives() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        // The caption input declares max_bytes = 16: an encoded caption
        // beyond that is refused before staging, leaving the previous
        // pending value coherent; a later valid injection replaces it.
        let stamp = |millis: u64| {
            phoxal::runtime::ObservationStamp::new(
                "test-caption",
                phoxal::runtime::ExecutionTime::from_nanos(millis * 1_000_000),
                ::std::option::Option::Some(41),
            )
        };
        let caption = |label: &str, millis: u64| {
            phoxal::runtime::Sample::new(
                monitor::Caption {
                    label: label.to_owned(),
                },
                stamp(millis),
            )
        };
        let mut host = Harness::<Monitor>::new(())?;
        host.inject_caption(caption("ok", 0))?;
        assert!(matches!(
            host.inject_caption(caption(&"x".repeat(64), 0)),
            Err(HarnessError::PendingFull {
                endpoint: "caption"
            })
        ));
        // The refused injection left the pending "ok" coherent: the next
        // release observes exactly that caption.
        host.advance_to(Duration::ZERO)?;
        let _ = host.beats();
        let state = host.seen().expect("state publication is retained");
        assert_eq!(state.observed_caption.as_deref(), Some("ok"));

        // A later valid injection replaces it and is observed whole.
        host.inject_caption(caption("second", 20))?;
        host.advance_to(Duration::from_millis(20))?;
        let _ = host.beats();
        let state = host.seen().expect("state publication is retained");
        assert_eq!(state.observed_caption.as_deref(), Some("second"));
        Ok(())
    }

    #[test]
    fn an_injected_latest_observation_preserves_revision_and_skew_policy() -> phoxal::Result<()> {
        use phoxal::runtime::Harness;
        use std::time::Duration;

        // The optional stamp revision travels with the retained
        // observation across cuts, and the existing bounded-forward-skew
        // policy decides freshness for a stamp ahead of the clock: within
        // 250 ms ahead is fresh evidence, beyond it is invalid.
        let mut host = Harness::<Monitor>::new(())?;

        // Ahead by 200 ms with revision 3: fresh, and the revision is
        // preserved into the handler's frozen cut.
        let ahead = phoxal::runtime::Sample::new(
            monitor::Reading { level: 11 },
            phoxal::runtime::ObservationStamp::new(
                "skewed-sensor",
                phoxal::runtime::ExecutionTime::from_nanos(200_000_000),
                ::std::option::Option::Some(3),
            ),
        );
        host.inject_level(ahead)?;
        host.advance_to(Duration::ZERO)?;
        let _ = host.beats();
        let state = host.seen().expect("state publication is retained");
        assert_eq!(
            state.last_fresh_level,
            Some(11),
            "bounded forward skew is fresh"
        );
        assert_eq!(
            state.last_level_revision,
            Some(3),
            "the revision travels with the stamp"
        );

        // Ahead by 300 ms: beyond the bounded skew, invalid evidence —
        // never treated as fresh.
        let beyond = phoxal::runtime::Sample::new(
            monitor::Reading { level: 12 },
            phoxal::runtime::ObservationStamp::new(
                "skewed-sensor",
                phoxal::runtime::ExecutionTime::from_nanos(500_000_000),
                ::std::option::Option::Some(4),
            ),
        );
        host.inject_level(beyond)?;
        host.advance_to(Duration::from_millis(20))?;
        let _ = host.beats();
        let state = host.seen().expect("state publication is retained");
        assert_eq!(
            state.last_fresh_level, None,
            "a stamp beyond the bounded forward skew is invalid evidence"
        );
        // The invalid evidence is still the retained observation with its
        // own provenance; it simply never reads as fresh.
        assert_eq!(state.last_capture_ns, Some(500_000_000));
        assert_eq!(state.last_level_revision, Some(4));
        Ok(())
    }

    #[test]
    fn reset_discards_pending_and_retained_observations() -> phoxal::Result<()> {
        use phoxal::runtime::Harness;
        use std::time::Duration;

        let mut host = Harness::<Monitor>::new(())?;
        host.inject_level(stamped_level(7, 0))?;
        host.advance_to(Duration::ZERO)?;
        assert_eq!(host.derived().map(|r| r.level), Some(7));

        // A still-pending injection and the retained accepted output both
        // disappear; only legitimate bootstrap state remains.
        host.inject_level(stamped_level(9, 20))?;
        host.reset(())?;
        let fresh = host.seen().expect("bootstrap state is retained");
        assert_eq!(fresh.last_fresh_level, None);
        assert_eq!(fresh.last_capture_ns, None);
        assert_eq!(fresh.last_source, None);
        assert_eq!(host.derived(), None, "the retained latest output is gone");
        // The discarded pending injection never arrives.
        host.advance_to(Duration::from_millis(20))?;
        let after = host.seen().expect("state publication is retained");
        assert_eq!(after.last_fresh_level, None);
        Ok(())
    }

    #[test]
    fn a_rejected_candidate_exposes_no_new_latest_output() -> phoxal::Result<()> {
        use phoxal::runtime::{Harness, HarnessError};
        use std::time::Duration;

        // The monitor emits one beat per release and retains at most four
        // undrained: releases 0-60 fill the bound, so the fifth candidate
        // (staged fresh level for the 80 ms release, which would publish a
        // new derived value and update state) is rejected whole — no new
        // latest output and no state update from it.
        let mut host = Harness::<Monitor>::new(())?;
        for (level, millis) in [(1_u64, 0_u64), (2, 20), (3, 40), (4, 60)] {
            host.inject_level(stamped_level(level, millis))?;
            host.advance_to(Duration::from_millis(millis))?;
            assert_eq!(host.derived().map(|r| r.level), Some(level));
        }

        host.inject_level(stamped_level(5, 80))?;
        let failure = host.advance_to(Duration::from_millis(80)).unwrap_err();
        assert!(
            failure.to_string().contains("retained"),
            "the rejection is the beats retention bound: {failure}"
        );
        assert_eq!(
            host.derived().map(|r| r.level),
            Some(4),
            "the rejected candidate published no new latest value"
        );
        let state = host.seen().expect("state publication is retained");
        assert_eq!(state.last_fresh_level, Some(4));
        assert!(matches!(
            host.advance_to(Duration::from_millis(100)),
            Err(HarnessError::Terminal { .. })
        ));
        Ok(())
    }

    #[test]
    fn authored_runtimes_bind_the_type_only_launch_spelling() {
        // A privately owned runtime (Countdown) and a publicly declared one
        // (Echo) both satisfy the launcher through the attachment alone;
        // authored code never constructs a value or names the adapter.
        fn assert_launches<R: phoxal::runtime::LaunchedRuntime>() {}
        assert_launches::<Countdown>();
        assert_launches::<Echo>();
        assert_launches::<Gate>();
    }

    #[test]
    fn reset_reconstructs_a_fresh_instance_under_a_new_fence() -> phoxal::Result<()> {
        let mut owner = CountdownOwner::new(phoxal_runtime_countdown::Adapter::new(), at(0), ())?;

        // Complete one job so the retained result makes its identifier
        // immediately unrepeatable.
        let outputs = accept_countdown(
            &mut owner,
            0,
            0,
            &countdown_inputs(vec![ordered(1, start(7, 20))], Vec::new()),
        )?;
        assert!(matches!(
            outputs.start_replies[0].response(),
            countdown::StartResponse::Accepted
        ));
        let outputs =
            accept_countdown(&mut owner, 40, 1, &countdown_inputs(Vec::new(), Vec::new()))?;
        assert_eq!(outputs.finished.len(), 1);
        let outputs = accept_countdown(
            &mut owner,
            60,
            2,
            &countdown_inputs(vec![ordered(1, start(7, 100))], Vec::new()),
        )?;
        assert!(
            matches!(
                outputs.start_replies[0].response(),
                countdown::StartResponse::Invalid
            ),
            "the completed job's identifier is unrepeatable before reset"
        );

        // Reset reconstructs the initialized instance under a fresh
        // execution fence: the invocation index restarts and no candidate
        // from the old fence is resumed.
        owner.reset(at(1_000), ())?;
        assert_eq!(owner.status(), RuntimeStatus::Ready);
        assert_eq!(owner.next_invocation().index(), 0);

        // The reconstructed instance has no memory of the completed job:
        // the same identifier is accepted again from the new fence.
        let outputs = accept_countdown(
            &mut owner,
            1_020,
            0,
            &countdown_inputs(vec![ordered(1, start(7, 100))], Vec::new()),
        )?;
        assert!(
            matches!(
                outputs.start_replies[0].response(),
                countdown::StartResponse::Accepted
            ),
            "old job state did not survive the reset"
        );
        Ok(())
    }

    #[test]
    fn no_authored_output_gains_initial_publication_authority() -> phoxal::Result<()> {
        // Initialization publishes retained state only. The Valve fixture
        // carries a leased projected setpoint, so this test has a real
        // subject: its compiled binding carries the lease WITHOUT the
        // bootstrap mark. The runner's bootstrap phase consumes exactly
        // this metadata and drops every non-state projection, and a
        // withdrawn projection encodes as an explicit withdrawal control
        // rather than a value — neither mechanism is directly assertable
        // from this tier (the runner filter and the wire control kind are
        // internal), so the executable proof here is the authored metadata
        // that feeds them.
        let target_field = <phoxal_runtime_valve::Adapter as OutputBindings>::FIELDS
            .iter()
            .find(|field| field.name == "target")
            .expect("the leased target binding exists");
        assert_eq!(target_field.valid_for_ms, Some(100));
        assert!(
            !target_field.bootstrap,
            "a leased projection never publishes initially"
        );
        assert_eq!(
            target_field.kind,
            OutputKind::Setpoint,
            "the leased projection binds as a setpoint output"
        );

        // The same invariant holds across every other fixture: no output
        // that carries a lease is marked for bootstrap.
        let all_bindings: [&'static [phoxal::runtime::outputs::OutputField]; 6] = [
            <phoxal_runtime_countdown::Adapter as OutputBindings>::FIELDS,
            <phoxal_runtime_echo::Adapter as OutputBindings>::FIELDS,
            <phoxal_runtime_gate::Adapter as OutputBindings>::FIELDS,
            <phoxal_runtime_monitor::Adapter as OutputBindings>::FIELDS,
            <phoxal_runtime_ticker::Adapter as OutputBindings>::FIELDS,
            <phoxal_runtime_flaky::Adapter as OutputBindings>::FIELDS,
        ];
        for bindings in all_bindings {
            for field in bindings {
                if field.valid_for_ms.is_some() {
                    assert!(
                        !field.bootstrap,
                        "leased output `{}` must not publish initially",
                        field.name
                    );
                }
            }
        }
        phoxal_runtime_valve::Adapter::retain_artifact_metadata();
        Ok(())
    }

    #[test]
    fn handler_only_participant_dispatches_without_a_step() -> phoxal::Result<()> {
        let mut owner = EchoOwner::new(phoxal_runtime_echo::Adapter::new(), at(0), ())?;
        let mut inputs = <echo::echo_api::Inputs as InputSnapshot>::empty();
        inputs.poke = Commands::new(vec![
            ordered(
                1,
                echo::PokeRequest {
                    payload: "first".to_owned(),
                },
            ),
            ordered(
                2,
                echo::PokeRequest {
                    payload: "second".to_owned(),
                },
            ),
        ]);

        let context = StepContext::first(at(0), ExecutionDuration::from_millis(50));
        let outputs = owner.accept(&context, &inputs)?.into_outputs();
        assert_eq!(outputs.poke_replies.len(), 2);
        assert_eq!(outputs.poke_replies[0].response().heard, "first");
        assert_eq!(outputs.poke_replies[1].response().heard, "second");

        // An idle invocation without a step succeeds and produces nothing.
        let inputs = <echo::echo_api::Inputs as InputSnapshot>::empty();
        let context = StepContext::from_previous(
            at(50),
            ExecutionDuration::from_millis(50),
            Some(at(0)),
            0,
            1,
        );
        let outputs = owner.accept(&context, &inputs)?.into_outputs();
        assert!(outputs.poke_replies.is_empty());
        assert_eq!(owner.status(), RuntimeStatus::Ready);
        Ok(())
    }

    #[test]
    fn step_only_runtime_advances_without_handlers() -> phoxal::Result<()> {
        // A runtime with no operations and no queued inputs still binds and
        // dispatches through the same generated surface; its step advances
        // ordinary fields each accepted invocation.
        let adapter = phoxal_runtime_ticker::Adapter::new();
        let state = initialize(&adapter, at(0), ())?;
        assert_eq!(state.count, 0);

        let empty = <ticker::ticker_api::Inputs as InputSnapshot>::empty();
        let (state, _) = crate::runtime_authoring::accept_candidate(
            &adapter,
            &later_step(20, 1),
            state,
            &empty,
        )?;
        assert_eq!(state.count, 1);
        let (state, _) = crate::runtime_authoring::accept_candidate(
            &adapter,
            &later_step(40, 2),
            state,
            &empty,
        )?;
        assert_eq!(state.count, 2);
        phoxal_runtime_ticker::Adapter::retain_artifact_metadata();
        Ok(())
    }

    #[test]
    fn initializer_configuration_types_resolve_under_every_supported_spelling() -> phoxal::Result<()>
    {
        // A plain identifier, a nested path, a `self`-qualified path, and a
        // crate-rooted path all resolve from the generated adapter module
        // one scope below this one and initialize from real values.
        let gate = initialize(
            &phoxal_runtime_gate::Adapter::new(),
            at(0),
            GateSettings { threshold: 5 },
        )?;
        assert_eq!(gate.threshold, 5);

        let nested = initialize(
            &phoxal_runtime_nested_gate::Adapter::new(),
            at(0),
            gate_config::Settings { threshold: 6 },
        )?;
        assert_eq!(nested.threshold, 6);

        let explicit_self = initialize(
            &phoxal_runtime_self_gate::Adapter::new(),
            at(0),
            self::gate_config::Settings { threshold: 7 },
        )?;
        assert_eq!(explicit_self.threshold, 7);

        let rooted = initialize(
            &phoxal_runtime_rooted_gate::Adapter::new(),
            at(0),
            super::gate_config::Settings { threshold: 9 },
        )?;
        assert_eq!(rooted.threshold, 9);

        phoxal_runtime_gate::Adapter::retain_artifact_metadata();
        phoxal_runtime_nested_gate::Adapter::retain_artifact_metadata();
        phoxal_runtime_self_gate::Adapter::retain_artifact_metadata();
        phoxal_runtime_rooted_gate::Adapter::retain_artifact_metadata();
        Ok(())
    }

    #[test]
    fn latest_and_leased_reads_survive_output_staging_in_one_invocation() -> phoxal::Result<()> {
        // The fixture's step binds both frozen reads before staging an
        // output through the same context and inspects them afterward; this
        // test observes the results of that authored spelling.
        let adapter = phoxal_runtime_monitor::Adapter::new();
        let state = initialize(&adapter, at(0), ())?;

        let mut inputs = <monitor::monitor_api::Inputs as InputSnapshot>::empty();
        inputs.level = Latest::new(
            monitor::Reading { level: 7 },
            ObservationStamp::new("level-producer", at(0), None),
        );
        inputs.grant = Setpoint::new(monitor::Reading { level: 4 }, at(0), 100);
        let (state, outputs) =
            crate::runtime_authoring::accept_candidate(&adapter, &first_step(0), state, &inputs)?;
        assert_eq!(state.last_fresh_level, Some(7));
        assert_eq!(state.last_valid_grant, Some(4));
        assert_eq!(outputs.beats.len(), 1);

        // An expired lease withdraws authority even though the value is
        // still present; staging through the same context is unaffected.
        let mut inputs = <monitor::monitor_api::Inputs as InputSnapshot>::empty();
        inputs.grant = Setpoint::new(monitor::Reading { level: 4 }, at(0), 100);
        let (state, outputs) = crate::runtime_authoring::accept_candidate(
            &adapter,
            &later_step(500, 1),
            state,
            &inputs,
        )?;
        assert_eq!(state.last_fresh_level, None);
        assert_eq!(
            state.last_valid_grant, None,
            "lease expiry withdraws authority"
        );
        assert_eq!(outputs.beats.len(), 1);
        Ok(())
    }

    /// An output admission that reserves by encoding the complete candidate
    /// batch, mirroring the whole-batch boundary the transport adapter
    /// applies between dispatch and publication, including reply port
    /// resolution against the compiled input records. The counter records
    /// how many candidates actually reached reservation.
    struct EncodingAdmission {
        reserve_calls: usize,
    }

    impl OutputAdmission<flaky::flaky_api::Outputs> for EncodingAdmission {
        type Reservation = ();

        fn reserve(
            &mut self,
            outputs: &flaky::flaky_api::Outputs,
        ) -> phoxal::Result<Self::Reservation> {
            self.reserve_calls += 1;
            let context = StepContext::first(ExecutionTime::from_nanos(0), PERIOD_20MS);
            outputs
                .encode_transport(
                    context,
                    &|name| (name == "arm").then_some(arm_port()).flatten(),
                    "admission-test",
                )
                .map(|_| ())
        }
    }

    #[test]
    fn admission_rejection_retires_the_owner_without_publishing() -> phoxal::Result<()> {
        let mut owner = FlakyOwner::new(phoxal_runtime_flaky::Adapter::new(), at(0), ())?;

        // A within-capacity batch passes whole-batch reservation and is
        // accepted with its outputs exposed.
        let mut inputs = <flaky::flaky_api::Inputs as InputSnapshot>::empty();
        inputs.arm = Commands::new(vec![ordered(
            1,
            flaky::TryRequest {
                explode: false,
                sparks: 2,
            },
        )]);
        let mut admission = EncodingAdmission { reserve_calls: 0 };
        let accepted = owner.accept_with(&first_step(0), &inputs, &mut admission)?;
        assert_eq!(accepted.into_outputs().sparks.len(), 2);
        assert_eq!(owner.next_invocation().index(), 1);
        assert_eq!(admission.reserve_calls, 1);

        // An over-capacity batch fails reservation as a whole: the candidate
        // is discarded, the owner retires terminally, and no part of the
        // batch was published.
        let mut inputs = <flaky::flaky_api::Inputs as InputSnapshot>::empty();
        inputs.arm = Commands::new(vec![ordered(
            1,
            flaky::TryRequest {
                explode: false,
                sparks: 3,
            },
        )]);
        let mut admission = EncodingAdmission { reserve_calls: 0 };
        let rejection = owner.accept_with(&later_step(20, 1), &inputs, &mut admission);
        let error = match rejection {
            Ok(_) => panic!("an over-capacity batch must fail reservation"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("batch exceeds item count bound: 3 > 2"),
            "the rejection is the sparks capacity bound: {error}"
        );
        assert_eq!(owner.status(), RuntimeStatus::Failed);
        assert_eq!(
            owner.next_invocation().index(),
            1,
            "a rejected candidate consumes no invocation index"
        );
        assert_eq!(admission.reserve_calls, 1);

        // A failed execution never resumes. The owner checks `Failed`
        // before the invocation index, so the matching index isolates the
        // refusal as the failed-state verdict; it happens before any
        // dispatch or admission, so reservation is never reached again.
        let inputs = <flaky::flaky_api::Inputs as InputSnapshot>::empty();
        let mut admission = EncodingAdmission { reserve_calls: 0 };
        let refusal = owner.accept_with(&later_step(40, 1), &inputs, &mut admission);
        let error = match refusal {
            Ok(_) => panic!("a failed execution never resumes"),
            Err(error) => error,
        };
        assert!(
            matches!(
                error.downcast_ref::<InvocationError>(),
                Some(InvocationError::Failed)
            ),
            "the terminal refusal names the failed execution, not an index or admission error: {error}"
        );
        assert_eq!(
            admission.reserve_calls, 0,
            "the terminal retry never reaches admission"
        );
        assert_eq!(owner.next_invocation().index(), 1);
        Ok(())
    }

    #[test]
    fn queue_capacity_one_drains_while_latest_retains() -> phoxal::Result<()> {
        let adapter = phoxal_runtime_monitor::Adapter::new();

        // The authored capacity-one policy is compiled into the generated
        // input record, not only into a hand-built SDK capacity.
        let triggers_field = <monitor::monitor_api::Inputs as InputSet>::FIELDS
            .iter()
            .find(|field| field.name == "triggers")
            .expect("the triggers input field exists");
        assert_eq!(triggers_field.max_items, Some(1));

        // A capacity-one queue cannot admit a two-item batch: overflow is a
        // visible admission failure, never a silent collapse to the newest.
        let two = vec![
            Sample::new(monitor::Trigger { sequence: 1 }, stamp(1)),
            Sample::new(monitor::Trigger { sequence: 2 }, stamp(1)),
        ];
        let capacity = Capacity::new(1, 256).expect("a positive capacity");
        assert!(Samples::bounded(two, 32, capacity).is_err());

        // One trigger is consumed by its handler.
        let state = initialize(&adapter, at(0), ())?;
        let mut inputs = <monitor::monitor_api::Inputs as InputSnapshot>::empty();
        inputs.triggers = Samples::new(vec![Sample::new(
            monitor::Trigger { sequence: 1 },
            stamp(0),
        )]);
        inputs.level = Latest::new(
            monitor::Reading { level: 7 },
            ObservationStamp::new("level-producer", at(0), None),
        );
        let (state, _) =
            crate::runtime_authoring::accept_candidate(&adapter, &first_step(0), state, &inputs)?;
        assert_eq!(state.consumed, 1);
        assert_eq!(state.last_fresh_level, Some(7));

        // The next cut carries an empty queue and the same retained reading:
        // the queue does not redeliver its consumed item, while the latest
        // value is still present and fresh.
        let mut inputs = <monitor::monitor_api::Inputs as InputSnapshot>::empty();
        inputs.level = Latest::new(
            monitor::Reading { level: 7 },
            ObservationStamp::new("level-producer", at(0), None),
        );
        let (state, _) = crate::runtime_authoring::accept_candidate(
            &adapter,
            &later_step(20, 1),
            state,
            &inputs,
        )?;
        assert_eq!(state.consumed, 1, "the drained queue retains no item");
        assert_eq!(state.last_fresh_level, Some(7));

        // Freshness governs the latest view: a reading older than the
        // declared bound is not fresh even though it is still present.
        let mut inputs = <monitor::monitor_api::Inputs as InputSnapshot>::empty();
        inputs.level = Latest::new(
            monitor::Reading { level: 7 },
            ObservationStamp::new("level-producer", at(0), None),
        );
        let (state, _) = crate::runtime_authoring::accept_candidate(
            &adapter,
            &later_step(500, 2),
            state,
            &inputs,
        )?;
        assert_eq!(state.last_fresh_level, None);

        let mut inputs = <monitor::monitor_api::Inputs as InputSnapshot>::empty();
        inputs.level = Latest::new(
            monitor::Reading { level: 9 },
            ObservationStamp::new("level-producer", at(500), None),
        );
        let (state, _) = crate::runtime_authoring::accept_candidate(
            &adapter,
            &later_step(520, 3),
            state,
            &inputs,
        )?;
        assert_eq!(state.last_fresh_level, Some(9));
        assert_eq!(state.consumed, 1);
        Ok(())
    }

    fn stamp(millis: u64) -> ObservationStamp {
        ObservationStamp::new("trigger-producer", at(millis), None)
    }

    #[test]
    fn handler_failure_is_terminal_and_later_invocations_are_refused() -> phoxal::Result<()> {
        let mut owner = FlakyOwner::new(phoxal_runtime_flaky::Adapter::new(), at(0), ())?;

        // The first arm succeeds and arms the deliberate failure.
        let mut inputs = <flaky::flaky_api::Inputs as InputSnapshot>::empty();
        inputs.arm = Commands::new(vec![ordered(
            1,
            flaky::TryRequest {
                explode: true,
                sparks: 0,
            },
        )]);
        let outputs = owner.accept(&first_step(0), &inputs)?.into_outputs();
        assert!(matches!(
            outputs.arm_replies[0].response(),
            flaky::TryResponse::Armed
        ));
        assert!(outputs.sparks.is_empty());

        // The next arm fails: the handler error faults the execution.
        let mut inputs = <flaky::flaky_api::Inputs as InputSnapshot>::empty();
        inputs.arm = Commands::new(vec![ordered(
            1,
            flaky::TryRequest {
                explode: false,
                sparks: 0,
            },
        )]);
        assert!(owner.accept(&later_step(20, 1), &inputs).is_err());
        assert_eq!(owner.status(), RuntimeStatus::Failed);

        // A failed execution never resumes: later invocations are refused
        // and produce no outputs.
        let inputs = <flaky::flaky_api::Inputs as InputSnapshot>::empty();
        assert!(owner.accept(&later_step(40, 2), &inputs).is_err());
        assert_eq!(owner.status(), RuntimeStatus::Failed);
        Ok(())
    }

    /// Resolves the flaky contract's reply port from its compiled input
    /// record, the way the transport adapter's manifest resolution does.
    fn arm_port() -> Option<phoxal::contracts::MethodSignature> {
        <flaky::flaky_api::Inputs as InputSet>::FIELDS
            .iter()
            .find(|field| field.name == "arm")
            .and_then(|field| field.port_signature)
    }

    #[test]
    fn oversized_output_batch_is_rejected_as_a_whole() -> phoxal::Result<()> {
        let adapter = phoxal_runtime_flaky::Adapter::new();
        let state = initialize(&adapter, at(0), ())?;

        // A staged batch that exceeds the declared item capacity fails
        // complete-batch validation at the output boundary: nothing is
        // partially published. The reply port is resolved so the failure
        // proven here is the sparks capacity bound itself, not missing
        // reply metadata.
        let mut inputs = <flaky::flaky_api::Inputs as InputSnapshot>::empty();
        inputs.arm = Commands::new(vec![ordered(
            1,
            flaky::TryRequest {
                explode: false,
                sparks: 3,
            },
        )]);
        let candidate =
            crate::runtime_authoring::accept_candidate(&adapter, &later_step(0, 1), state, &inputs);
        let error = match candidate {
            Ok(_) => panic!("three staged items must exceed the declared capacity of two"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("batch exceeds item count bound: 3 > 2"),
            "the rejection is the sparks capacity bound: {error}"
        );
        Ok(())
    }

    #[test]
    fn generated_artifact_record_carries_the_declared_contract() {
        // The registered spec and the linked artifact record exist for the
        // authored runtime with the authored struct as its state type. The
        // authored path defaults the host deadlines it does not declare.
        let spec = <phoxal_runtime_countdown::Adapter as RegisteredRuntime>::SPEC;
        assert_eq!(spec.period, PERIOD_20MS);
        assert_eq!(spec.timeout, ExecutionDuration::from_millis(100));
        assert_eq!(spec.init_timeout, ExecutionDuration::from_millis(1_000));
        phoxal_runtime_countdown::Adapter::retain_artifact_metadata();
        phoxal_runtime_echo::Adapter::retain_artifact_metadata();
        phoxal_runtime_monitor::Adapter::retain_artifact_metadata();
        phoxal_runtime_flaky::Adapter::retain_artifact_metadata();
    }

    use super::Pilot;
    use super::modes;
    use super::pilot::AskResponse;
    use phoxal::contracts::ProstPayload as _;
    use phoxal::runtime::behavior::TreeStatus;
    use phoxal::runtime::input::{TransportCallCompletion, TransportInputSink};

    type PilotOwner = RuntimeOwner<super::phoxal_runtime_pilot::Adapter>;
    const PILOT_PERIOD: ExecutionDuration = ExecutionDuration::from_millis(10);

    fn pilot_context(millis: u64, index: u64) -> StepContext {
        if index == 0 {
            StepContext::first(ExecutionTime::from_nanos(millis * 1_000_000), PILOT_PERIOD)
        } else {
            StepContext::from_previous(
                ExecutionTime::from_nanos(millis * 1_000_000),
                PILOT_PERIOD,
                Some(ExecutionTime::from_nanos(
                    millis.saturating_sub(10) * 1_000_000,
                )),
                0,
                index,
            )
        }
    }

    fn pilot_empty() -> super::pilot::pilot_api::Inputs {
        <super::pilot::pilot_api::Inputs as InputSnapshot>::empty()
    }

    fn pilot_completion(
        value: u64,
        ticket: u128,
    ) -> phoxal::Result<super::pilot::pilot_api::Inputs> {
        let mut inputs = pilot_empty();
        inputs.set_call_completions(vec![TransportCallCompletion {
            ticket,
            result: Ok(AskResponse { value }.encode_payload()?),
        }])?;
        Ok(inputs)
    }

    fn pilot_events(job_id: u64) -> super::pilot::pilot_api::Inputs {
        let mut inputs = pilot_empty();
        if let Err(error) = inputs.set_samples(
            "events",
            vec![phoxal::runtime::input::TransportSample {
                value: Box::new(super::pilot::ProbeEvent { job_id }),
                stamp: phoxal::runtime::ObservationStamp::new(
                    "probe-source",
                    ExecutionTime::from_nanos(0),
                    None,
                ),
            }],
            false,
        ) {
            panic!("the events sink accepts the batch: {error:#}");
        }
        inputs
    }

    fn pilot_command(
        field: &'static str,
        order: u64,
        request: Box<super::pilot::BeginRequest>,
    ) -> super::pilot::pilot_api::Inputs {
        let mut inputs = pilot_empty();
        if let Err(error) = inputs.set_commands(
            field,
            vec![phoxal::runtime::input::TransportCommand {
                order: phoxal::runtime::input::CommandOrder::new(
                    0,
                    0,
                    phoxal::runtime::input::CommandId::new(order),
                ),
                source: "probe-commander".to_owned(),
                request,
            }],
            64,
        ) {
            panic!("the command sink accepts the batch: {error:#}");
        }
        inputs
    }

    fn pilot_uncertain(ticket: u128) -> super::pilot::pilot_api::Inputs {
        let mut inputs = pilot_empty();
        if let Err(error) = inputs.set_call_completions(vec![TransportCallCompletion {
            ticket,
            result: Err(phoxal::runtime::input::RequestError::OutcomeUnknown(
                "the probe lost the remote outcome".to_owned(),
            )),
        }]) {
            panic!("the uncertain completion sink accepts the record: {error:#}");
        }
        inputs
    }

    type PilotAdapter = super::phoxal_runtime_pilot::Adapter;

    fn pilot_start(mode: u64) -> phoxal::Result<(PilotAdapter, Pilot)> {
        // One adapter value across every invocation: it owns the pending
        // call table, so a fresh adapter per step would drop ownership.
        let adapter = PilotAdapter::new();
        let pilot = initialize(&adapter, ExecutionTime::default(), mode)?;
        Ok((adapter, pilot))
    }

    /// The ticket one staged call actually drew, derived exactly as the
    /// SDK composes it from the adapter's execution epoch.
    fn pilot_ticket(adapter: &PilotAdapter, invocation: u64, operation: usize) -> u128 {
        match phoxal::runtime::outputs::compose_call_ticket(
            adapter.execution_epoch(),
            invocation,
            operation,
        ) {
            Ok(ticket) => ticket,
            Err(error) => panic!("the pilot's ticket space is representable: {error:#}"),
        }
    }

    fn pilot_step(
        adapter: &PilotAdapter,
        pilot: Pilot,
        millis: u64,
        index: u64,
        inputs: &super::pilot::pilot_api::Inputs,
    ) -> phoxal::Result<Pilot> {
        let (pilot, _outputs) = crate::runtime_authoring::accept_candidate(
            adapter,
            &pilot_context(millis, index),
            pilot,
            inputs,
        )?;
        Ok(pilot)
    }

    #[test]
    fn event_and_completion_cancellation_retire_every_owned_resource() -> phoxal::Result<()> {
        use phoxal::runtime::Runtime as _;
        for completion in [false, true] {
            let (adapter, mut pilot) = pilot_start(modes::DELAY)?;
            pilot.tick_mission = false;
            pilot.cancel_on_event = !completion;
            pilot.cancel_on_completion = completion;
            pilot.stage_direct = completion;
            pilot.commanded = Some(Pilot::mission_for(modes::CAPTURE_BEFORE_REPLY)?);
            pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
            adapter.accepted();
            assert_eq!(adapter.authoring.resources().captures.active_count(), 1);
            assert_eq!(
                adapter.authoring.resources().pending.len(),
                if completion { 2 } else { 1 }
            );
            let inputs = if completion {
                pilot_completion(7, pilot_ticket(&adapter, 0, 1))?
            } else {
                pilot_events(7)
            };
            pilot = pilot_step(&adapter, pilot, 10, 1, &inputs)?;
            adapter.accepted();
            assert_eq!(
                pilot.commanded.as_ref().unwrap().status(),
                TreeStatus::Cancelled
            );
            assert_eq!(adapter.authoring.resources().captures.active_count(), 0);
            assert_eq!(adapter.authoring.resources().pending.len(), 0);
            assert_eq!(
                adapter.behavior_diary().accepted().last().unwrap().status,
                TreeStatus::Cancelled
            );
        }
        Ok(())
    }

    #[test]
    fn handler_cancellation_retires_every_owned_resource() -> phoxal::Result<()> {
        use phoxal::runtime::Runtime as _;
        let (adapter, mut pilot) = pilot_start(modes::DELAY)?;
        pilot.tick_mission = false;
        pilot.commanded = Some(Pilot::mission_for(modes::CAPTURE_BEFORE_REPLY)?);
        pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
        adapter.accepted();
        assert_eq!(adapter.authoring.resources().captures.active_count(), 1);
        assert_eq!(adapter.authoring.resources().pending.len(), 1);
        let stop = pilot_command(
            "stop",
            1,
            Box::new(super::pilot::BeginRequest {
                job: 1,
                duration_ms: 20,
            }),
        );
        pilot = pilot_step(&adapter, pilot, 10, 1, &stop)?;
        adapter.accepted();
        assert_eq!(
            pilot.commanded.as_ref().unwrap().status(),
            TreeStatus::Cancelled
        );
        assert_eq!(
            adapter.authoring.resources().captures.active_count(),
            0,
            "handler cancellation releases capture"
        );
        assert_eq!(
            adapter.authoring.resources().pending.len(),
            1,
            "only explicit remote cancellation is still owned"
        );
        assert_eq!(
            adapter.behavior_diary().accepted().last().unwrap().status,
            TreeStatus::Cancelled
        );
        Ok(())
    }

    /// A delay stage anchors at its first admitted tick: intermediate
    /// ticks never re-anchor it, and it completes exactly at the boundary.
    #[test]
    fn delay_anchors_at_first_activation_and_completes_at_the_boundary() -> phoxal::Result<()> {
        let (adapter, mut pilot) = pilot_start(modes::DELAY)?;
        pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
        assert_eq!(pilot.mission.status(), TreeStatus::Running);
        pilot = pilot_step(&adapter, pilot, 50, 1, &pilot_empty())?;
        assert_eq!(pilot.mission.status(), TreeStatus::Running);
        pilot = pilot_step(&adapter, pilot, 90, 2, &pilot_empty())?;
        assert_eq!(pilot.mission.status(), TreeStatus::Running);
        pilot = pilot_step(&adapter, pilot, 100, 3, &pilot_empty())?;
        assert_eq!(pilot.mission.status(), TreeStatus::Succeeded);
        Ok(())
    }

    /// The `within` deadline is inclusive and wins at the boundary instant
    /// before any further child work.
    #[test]
    fn within_deadline_is_inclusive_and_preempts_child_effects() -> phoxal::Result<()> {
        let (adapter, mut pilot) = pilot_start(modes::DEADLINE)?;
        pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
        pilot = pilot_step(&adapter, pilot, 99, 1, &pilot_empty())?;
        assert_eq!(pilot.mission.status(), TreeStatus::Running);
        pilot = pilot_step(&adapter, pilot, 100, 2, &pilot_empty())?;
        assert_eq!(pilot.mission.status(), TreeStatus::TimedOut);
        assert!(
            pilot
                .mission
                .ended_cause()
                .is_some_and(|cause| cause.contains("deadline budget")),
            "the timeout retains its cause, got {:?}",
            pilot.mission.ended_cause()
        );
        assert_eq!(pilot.completions, 0, "no child effects ran at the boundary");
        Ok(())
    }

    /// A terminal tree stays terminal: later ticks replay no effects.
    #[test]
    fn terminal_trees_latch() -> phoxal::Result<()> {
        let (adapter, mut pilot) = pilot_start(modes::DELAY)?;
        pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
        pilot = pilot_step(&adapter, pilot, 100, 1, &pilot_empty())?;
        assert_eq!(pilot.mission.status(), TreeStatus::Succeeded);
        assert_eq!(pilot.completions, 1);
        pilot = pilot_step(&adapter, pilot, 120, 2, &pilot_empty())?;
        pilot = pilot_step(&adapter, pilot, 140, 3, &pilot_empty())?;
        assert_eq!(pilot.mission.status(), TreeStatus::Succeeded);
        assert_eq!(pilot.completions, 1, "a latched tree replays no effects");
        Ok(())
    }

    /// A tree-owned completion survives a skipped tree tick without ever
    /// reaching the direct completion handler, and is consumed by its leaf
    /// on a later tick.
    #[test]
    fn tree_owned_completions_survive_skipped_ticks_exclusively() -> phoxal::Result<()> {
        let (adapter, mut pilot) = pilot_start(modes::TREE_CALL)?;
        // Invocation 0: the leaf stages its call as operation 0.
        pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
        assert_eq!(pilot.mission.status(), TreeStatus::Running);
        // The tree does not tick on the arrival cut.
        let tree_ticket = pilot_ticket(&adapter, 0, 0);
        pilot.tick_mission = false;
        pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_completion(1, tree_ticket)?)?;
        assert_eq!(
            pilot.primary_replies + pilot.secondary_replies,
            0,
            "a tree-owned completion never reaches a direct handler"
        );
        assert_eq!(pilot.mission.status(), TreeStatus::Running);
        // The leaf consumes its own completion on the next tick.
        pilot.tick_mission = true;
        pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_completion(1, tree_ticket)?)?;
        assert_eq!(pilot.mission.status(), TreeStatus::Succeeded);
        assert_eq!(pilot.tree_seen, None, "the observation is the leaf's own");
        assert_eq!(
            pilot.primary_replies + pilot.secondary_replies,
            0,
            "the leaf's consumption is invisible to both handlers"
        );
        Ok(())
    }

    /// One completion reaches exactly one field's handler, never both.
    #[test]
    fn one_completion_reaches_exactly_one_field_handler() -> phoxal::Result<()> {
        let (adapter, mut pilot) = pilot_start(modes::TREE_CALL)?;
        pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
        // A foreign ticket is delivered to no owner at all.
        pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_completion(5, 9_999)?)?;
        assert_eq!(pilot.primary_replies + pilot.secondary_replies, 0);
        assert_eq!(pilot.mission.status(), TreeStatus::Running);
        // The direct field stages its own call on invocation 2 as
        // operation 0 (the tree's call was never staged: it was cancelled
        // by construction in this mode's tick order).
        pilot.stage_direct = true;
        pilot.direct_staged = false;
        pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_empty())?;
        // The direct field stages its call as invocation 2's operation 0.
        let direct_ticket = pilot_ticket(&adapter, 2, 0);
        pilot = pilot_step(&adapter, pilot, 30, 3, &pilot_completion(7, direct_ticket)?)?;
        assert_eq!(pilot.primary_replies, 1, "the direct owner consumed it");
        assert_eq!(pilot.secondary_replies, 0, "the other field did not");
        Ok(())
    }

    /// A direct call and a tree call in flight on different fields each
    /// own their own completion.
    #[test]
    fn concurrent_direct_and_tree_calls_own_their_completions() -> phoxal::Result<()> {
        let (adapter, mut pilot) = pilot_start(modes::CONCURRENT)?;
        // The tree stages its call first (operation 0); the step's direct
        // call follows (operation 1).
        pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
        assert_eq!(pilot.mission.status(), TreeStatus::Running);
        assert!(pilot.direct_staged);
        let direct_ticket = pilot_ticket(&adapter, 0, 1);
        let tree_ticket = pilot_ticket(&adapter, 0, 0);
        pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_completion(7, direct_ticket)?)?;
        assert_eq!(pilot.primary_replies, 1, "the direct completion arrived");
        assert_eq!(
            pilot.mission.status(),
            TreeStatus::Running,
            "the tree's own completion has not arrived yet"
        );
        pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_completion(1, tree_ticket)?)?;
        assert_eq!(pilot.mission.status(), TreeStatus::Succeeded);
        assert_eq!(pilot.primary_replies, 1, "no cross-delivery occurred");
        assert_eq!(pilot.secondary_replies, 0);
        Ok(())
    }

    /// Cancelling before the reply retires the tree and the late reply is
    /// delivered to no owner: the direct handlers never see a tree's
    /// ticket, and the latched tree stays cancelled.
    #[test]
    fn cancel_before_reply_retires_ownership_and_latches() -> phoxal::Result<()> {
        let (adapter, mut pilot) = pilot_start(modes::CANCEL)?;
        pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
        pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_empty())?;
        assert_eq!(pilot.mission.status(), TreeStatus::Running);
        pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_empty())?;
        assert_eq!(pilot.mission.status(), TreeStatus::Cancelled);
        assert!(
            pilot
                .mission
                .ended_cause()
                .is_some_and(|cause| cause.contains("cancelled")),
            "the cancellation retains its cause"
        );
        // The late reply belongs to a retired call: no owner consumes it.
        let retired_ticket = pilot_ticket(&adapter, 0, 0);
        pilot = pilot_step(
            &adapter,
            pilot,
            30,
            3,
            &pilot_completion(1, retired_ticket)?,
        )?;
        assert_eq!(pilot.mission.status(), TreeStatus::Cancelled);
        assert_eq!(
            pilot.primary_replies + pilot.secondary_replies,
            0,
            "a retired tree's reply reaches no owner"
        );
        Ok(())
    }

    /// An old execution's result never completes a fresh execution's
    /// tree: the fresh tree's ticket draws the fresh adapter's own epoch,
    /// so the aliased old ticket is refused while the fresh ticket still
    /// completes its call.
    #[test]
    fn old_execution_results_never_complete_fresh_trees() -> phoxal::Result<()> {
        let (adapter, mut pilot) = pilot_start(modes::TREE_CALL)?;
        let old_ticket = pilot_ticket(&adapter, 0, 0);
        pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
        let _ = pilot;

        let (fresh_adapter, mut fresh) = pilot_start(modes::TREE_CALL)?;
        fresh = pilot_step(&fresh_adapter, fresh, 0, 0, &pilot_empty())?;
        let fresh_ticket = pilot_ticket(&fresh_adapter, 0, 0);
        assert_ne!(
            old_ticket, fresh_ticket,
            "successive executions draw distinct ticket spaces"
        );
        // The old execution's result must not advance the fresh tree.
        fresh = pilot_step(
            &fresh_adapter,
            fresh,
            10,
            1,
            &pilot_completion(1, old_ticket)?,
        )?;
        assert_eq!(
            fresh.mission.status(),
            TreeStatus::Running,
            "an old execution's result completed the fresh tree"
        );
        assert_eq!(
            fresh.primary_replies + fresh.secondary_replies,
            0,
            "the aliased ticket reached no direct handler either"
        );
        // The fresh tree's own ticket still completes its call.
        fresh = pilot_step(
            &fresh_adapter,
            fresh,
            20,
            2,
            &pilot_completion(1, fresh_ticket)?,
        )?;
        assert_eq!(fresh.mission.status(), TreeStatus::Succeeded);
        Ok(())
    }

    /// Reinitialization through the runtime owner — the same adapter, not
    /// a fresh one — fences every prior ownership: pre-reset results reach
    /// no handler and no leaf, while the fresh era's own calls complete.
    #[test]
    fn owner_reset_fences_prior_ownership() -> phoxal::Result<()> {
        let adapter = super::phoxal_runtime_pilot::Adapter::new();
        let mut owner = PilotOwner::new(adapter, ExecutionTime::default(), modes::CONCURRENT)?;
        // Invocation 0 stages one direct call (operation 1) and one
        // tree-owned call (operation 0) under the old era.
        owner.accept(&pilot_context(0, 0), &pilot_empty())?;
        let old_epoch = owner.service().execution_epoch();
        let old_direct = phoxal::runtime::outputs::compose_call_ticket(old_epoch, 0, 1)?;
        let old_tree = phoxal::runtime::outputs::compose_call_ticket(old_epoch, 0, 0)?;

        // Reinitialize through the owner: the adapter survives, its epoch
        // advances, and its ledger is cleared.
        owner.reset(ExecutionTime::from_nanos(1_000), modes::CONCURRENT)?;
        assert_ne!(
            owner.service().execution_epoch(),
            old_epoch,
            "reinitialization draws a fresh execution epoch"
        );
        owner.accept(&pilot_context(0, 0), &pilot_empty())?;

        // Pre-reset results reach no owner in the fresh era.
        let mut late = pilot_empty();
        late.set_call_completions(vec![
            TransportCallCompletion {
                ticket: old_direct,
                result: Ok(AskResponse { value: 7 }.encode_payload()?),
            },
            TransportCallCompletion {
                ticket: old_tree,
                result: Ok(AskResponse { value: 1 }.encode_payload()?),
            },
        ])?;
        let outputs = owner.accept(&pilot_context(10, 1), &late)?.into_outputs();
        let report = outputs
            .report
            .last()
            .expect("the report publishes every accepted invocation");
        assert_eq!(report.primary_replies, 0, "an old-era direct result fired");
        assert!(
            matches!(report.mission_phase, super::pilot::MissionPhase::Running),
            "an old-era result advanced the fresh tree"
        );
        super::Result::Ok(())
    }

    /// The owner-level smoke: the pilot initializes through the runtime
    /// owner and its first invocation reports Running, proving the fixture
    /// composes with the standard owner path.
    #[test]
    fn pilot_runs_through_the_runtime_owner() -> phoxal::Result<()> {
        let adapter = super::phoxal_runtime_pilot::Adapter::new();
        let owner = PilotOwner::new(adapter, ExecutionTime::default(), modes::DELAY);
        assert!(owner.is_ok());
        Ok(())
    }

    /// Composition-surface battery over the pilot's direct-drive harness.
    mod composition {
        use super::modes;
        use super::{
            pilot_command, pilot_completion, pilot_empty, pilot_events, pilot_start, pilot_step,
            pilot_ticket, pilot_uncertain,
        };
        use phoxal::runtime::behavior::TreeStatus;

        /// A selector advances to its next branch after an expected domain
        /// refusal (the response predicate failed on a decoded response) and
        /// completes through the later branch.
        #[test]
        fn selector_falls_back_after_domain_refusal() -> phoxal::Result<()> {
            let (adapter, mut pilot) = pilot_start(modes::SELECTOR_FALLBACK)?;
            pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            // The decoded response fails the predicate: an eligible domain
            // refusal, so the selector skips the second branch's immediate
            // condition failure and reaches the final wait.
            let ticket = pilot_ticket(&adapter, 0, 0);
            pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_completion(1, ticket)?)?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Succeeded);
            Ok(())
        }

        /// A selector never tries its next branch after an uncertain remote
        /// outcome: the potentially executed effect stops the tree.
        #[test]
        fn selector_stops_after_uncertain_outcome() -> phoxal::Result<()> {
            let (adapter, mut pilot) = pilot_start(modes::SELECTOR_UNCERTAIN)?;
            pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            let ticket = pilot_ticket(&adapter, 0, 0);
            pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_uncertain(ticket))?;
            assert_eq!(
                pilot.mission.status(),
                TreeStatus::TimedOut,
                "an uncertain effect must not authorize the next branch"
            );
            assert!(
                pilot
                    .mission
                    .ended_cause()
                    .is_some_and(|cause| cause.contains("unknown")),
                "the retained cause names the uncertainty, got {:?}",
                pilot.mission.ended_cause()
            );
            // The tree latched terminal: the would-succeed branch never ran.
            pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::TimedOut);
            Ok(())
        }

        /// A guard that stops holding halts its child exactly once, retiring
        /// the child's call ownership so a late reply is discarded unowned.
        #[test]
        fn guard_halt_retires_child_ownership() -> phoxal::Result<()> {
            let (adapter, mut pilot) = pilot_start(modes::GUARD_HALT)?;
            pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            // The guard predicate stops holding; its child never sees the
            // reply below because its ticket was retired.
            pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Refused);
            assert!(
                pilot
                    .mission
                    .ended_cause()
                    .is_some_and(|cause| cause.contains("guard predicate")),
                "the guard's cause is retained, got {:?}",
                pilot.mission.ended_cause()
            );
            // The late reply of the retired ticket is unowned: no direct
            // handler observes it and the tree stays terminal.
            let ticket = pilot_ticket(&adapter, 0, 0);
            pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_completion(1, ticket)?)?;
            assert_eq!(pilot.mission.status(), TreeStatus::Refused);
            assert_eq!(pilot.primary_replies, 0, "the retired call never delivers");
            Ok(())
        }

        /// Cooperative parallel waits of different lengths all complete in
        /// declaration order without replaying a finished child.
        #[test]
        fn all_waits_cooperatively() -> phoxal::Result<()> {
            let (adapter, mut pilot) = pilot_start(modes::ALL_PARALLEL)?;
            pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_empty())?;
            assert_eq!(
                pilot.mission.status(),
                TreeStatus::Running,
                "one child finished, one still runs"
            );
            pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            pilot = pilot_step(&adapter, pilot, 30, 3, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Succeeded);
            Ok(())
        }

        /// A race resolves through its first terminal child and latches; the
        /// loser is halted locally.
        #[test]
        fn race_first_terminal_wins() -> phoxal::Result<()> {
            let (adapter, mut pilot) = pilot_start(modes::RACE)?;
            pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
            pilot = pilot_step(&adapter, pilot, 50, 1, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            pilot = pilot_step(&adapter, pilot, 100, 2, &pilot_empty())?;
            assert_eq!(
                pilot.mission.status(),
                TreeStatus::Succeeded,
                "the delay branch terminates first at its deadline"
            );
            pilot = pilot_step(&adapter, pilot, 120, 3, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Succeeded);
            Ok(())
        }

        /// A typed continuation consumes one decoded response exactly once and
        /// completes through the child it built.
        #[test]
        fn continuation_builds_child_from_one_response() -> phoxal::Result<()> {
            let (adapter, mut pilot) = pilot_start(modes::CONTINUATION)?;
            pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            let ticket = pilot_ticket(&adapter, 0, 0);
            pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_completion(5, ticket)?)?;
            assert_eq!(
                pilot.mission.status(),
                TreeStatus::Succeeded,
                "the factory consumed the decoded response and its child ran"
            );
            // The continuation ran once: later ticks replay nothing.
            pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Succeeded);
            Ok(())
        }

        /// Repeat runs exactly three fresh attempts: each consumes its own
        /// response, yields before the next attempt, and the final success
        /// latches.
        #[test]
        fn repeat_runs_three_fresh_attempts_with_yield() -> phoxal::Result<()> {
            let (adapter, mut pilot) = pilot_start(modes::REPEAT)?;
            // Attempt 0 submits at invocation 0.
            pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            let ticket = pilot_ticket(&adapter, 0, 0);
            pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_completion(0, ticket)?)?;
            assert_eq!(
                pilot.mission.status(),
                TreeStatus::Running,
                "the repeat yields after a successful attempt"
            );
            // Attempt 1 starts no earlier than the next invocation.
            pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            let ticket = pilot_ticket(&adapter, 2, 0);
            pilot = pilot_step(&adapter, pilot, 30, 3, &pilot_completion(1, ticket)?)?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            pilot = pilot_step(&adapter, pilot, 40, 4, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            let ticket = pilot_ticket(&adapter, 4, 0);
            pilot = pilot_step(&adapter, pilot, 50, 5, &pilot_completion(2, ticket)?)?;
            assert_eq!(
                pilot.mission.status(),
                TreeStatus::Succeeded,
                "the third successful attempt completes the repeat"
            );
            Ok(())
        }

        /// A repeat child is genuinely fresh: the previous attempt's response
        /// value does not satisfy the new attempt's predicate.
        #[test]
        fn repeat_attempts_have_fresh_domain_identity() -> phoxal::Result<()> {
            let (adapter, mut pilot) = pilot_start(modes::REPEAT)?;
            pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
            let ticket = pilot_ticket(&adapter, 0, 0);
            pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_completion(0, ticket)?)?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            // Attempt 1 expects value 1; replaying value 0 is a domain refusal
            // that ends the repeat rather than retrying.
            pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_empty())?;
            let ticket = pilot_ticket(&adapter, 2, 0);
            pilot = pilot_step(&adapter, pilot, 30, 3, &pilot_completion(0, ticket)?)?;
            assert_eq!(
                pilot.mission.status(),
                TreeStatus::Refused,
                "the fresh attempt's predicate rejects the stale value"
            );
            Ok(())
        }

        /// A custom action runs incrementally, succeeds on its own predicate,
        /// and runs its action-local halt hook exactly once.
        #[test]
        fn custom_action_runs_and_halts_once() -> phoxal::Result<()> {
            let _guard = crate::runtime_authoring::ACTION_HALT_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            crate::runtime_authoring::ACTION_HALT
                .store(false, std::sync::atomic::Ordering::Relaxed);
            let (adapter, mut pilot) = pilot_start(modes::ACTION)?;
            pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Succeeded);
            assert!(
                crate::runtime_authoring::ACTION_HALT.load(std::sync::atomic::Ordering::Relaxed),
                "the halt hook ran when the action ended"
            );
            Ok(())
        }

        /// A capture activated before its call retains an event that arrives
        /// after the reply; the ordinary handler observes the same event.
        #[test]
        fn capture_waits_for_event_after_reply() -> phoxal::Result<()> {
            let (adapter, mut pilot) = pilot_start(modes::CAPTURE_AFTER_REPLY)?;
            pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            let ticket = pilot_ticket(&adapter, 0, 0);
            pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_completion(1, ticket)?)?;
            assert_eq!(
                pilot.mission.status(),
                TreeStatus::Running,
                "the wait runs with no event captured yet"
            );
            pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_events(7))?;
            assert_eq!(pilot.mission.status(), TreeStatus::Succeeded);
            assert_eq!(
                pilot.event_count, 1,
                "the ordinary handler observed the same event as the capture"
            );
            Ok(())
        }

        /// An event arriving before the reply is retained by the capture and
        /// satisfies the later wait in the reply's invocation.
        #[test]
        fn capture_retains_event_arriving_before_reply() -> phoxal::Result<()> {
            let (adapter, mut pilot) = pilot_start(modes::CAPTURE_BEFORE_REPLY)?;
            pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
            pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_events(7))?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            assert_eq!(pilot.event_count, 1, "the handler ran at the event's cut");
            let ticket = pilot_ticket(&adapter, 0, 0);
            pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_completion(1, ticket)?)?;
            assert_eq!(
                pilot.mission.status(),
                TreeStatus::Succeeded,
                "the retained event satisfies the wait without a new arrival"
            );
            Ok(())
        }

        /// The activation cut's own events are excluded: the capture begins
        /// strictly after the invocation that activated it.
        #[test]
        fn capture_excludes_events_of_its_activation_cut() -> phoxal::Result<()> {
            let (adapter, mut pilot) = pilot_start(modes::CAPTURE_AFTER_REPLY)?;
            // The activation invocation also carries the event: excluded from
            // the capture, while the ordinary handler still observes it.
            pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_events(7))?;
            assert_eq!(pilot.event_count, 1, "the handler still observed it");
            let ticket = pilot_ticket(&adapter, 0, 0);
            pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_completion(1, ticket)?)?;
            assert_eq!(
                pilot.mission.status(),
                TreeStatus::Running,
                "the excluded event never satisfies the wait"
            );
            pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_events(7))?;
            assert_eq!(pilot.mission.status(), TreeStatus::Succeeded);
            Ok(())
        }

        /// The command-triggered mission lifecycle: a start command is
        /// accepted into a fresh mission, a second start while running
        /// returns Busy instead of silently replacing it, and an explicit
        /// stop command halts the tree locally before issuing one correlated
        /// remote cancellation whose outcome is distinguished.
        #[test]
        fn command_triggered_mission_handles_busy_replacement_and_cancellation()
        -> phoxal::Result<()> {
            let (adapter, mut pilot) = pilot_start(modes::DELAY)?;
            // No commanded mission exists initially.
            assert!(pilot.commanded.is_none());

            // First start: accepted, constructing the mission with the
            // command's own domain identity (job 11).
            let begin = pilot_command(
                "begin",
                1,
                Box::new(crate::runtime_authoring::pilot::BeginRequest {
                    job: 11,
                    duration_ms: 20,
                }),
            );
            pilot = pilot_step(&adapter, pilot, 0, 0, &begin)?;
            assert!(pilot.commanded.is_some(), "the first start was accepted");
            assert_eq!(
                pilot
                    .commanded
                    .as_ref()
                    .and_then(|mission| (mission.status() == TreeStatus::Running).then_some(())),
                Some(()),
                "the commanded mission runs"
            );

            // The mission's own call is tree-owned: its ticket lives under
            // the commanded tree, answered at the next invocation.
            let ticket = pilot_ticket(&adapter, 0, 0);
            pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_completion(11, ticket)?)?;

            // Second start while the mission still runs (its delay anchored
            // at t=10 ms completes at t=30 ms): Busy, untouched.
            let second = pilot_command(
                "begin",
                2,
                Box::new(crate::runtime_authoring::pilot::BeginRequest {
                    job: 12,
                    duration_ms: 20,
                }),
            );
            pilot = pilot_step(&adapter, pilot, 20, 2, &second)?;
            assert_eq!(
                pilot
                    .commanded
                    .as_ref()
                    .map_or(TreeStatus::Running, |mission| mission.status()),
                TreeStatus::Running,
                "the Busy reply replaced the running mission with nothing"
            );
            // The original mission completes its own delay at t=30 ms.
            pilot = pilot_step(&adapter, pilot, 30, 3, &pilot_empty())?;
            assert_eq!(
                pilot
                    .commanded
                    .as_ref()
                    .map_or(TreeStatus::Running, |mission| mission.status()),
                TreeStatus::Succeeded,
                "the first mission finished with its own identity"
            );

            // A terminal mission is replaceable by the next command.
            let third = pilot_command(
                "begin",
                3,
                Box::new(crate::runtime_authoring::pilot::BeginRequest {
                    job: 13,
                    duration_ms: 20,
                }),
            );
            pilot = pilot_step(&adapter, pilot, 40, 4, &third)?;
            assert_eq!(
                pilot
                    .commanded
                    .as_ref()
                    .map_or(TreeStatus::Running, |mission| mission.status()),
                TreeStatus::Running,
                "the replacement mission carries the new command"
            );

            // Stop while the replacement runs: local halt first, then one
            // correlated remote cancellation.
            let stop = pilot_command(
                "stop",
                4,
                Box::new(crate::runtime_authoring::pilot::BeginRequest {
                    job: 13,
                    duration_ms: 20,
                }),
            );
            pilot = pilot_step(&adapter, pilot, 50, 5, &stop)?;
            assert_eq!(
                pilot
                    .commanded
                    .as_ref()
                    .map_or(TreeStatus::Running, |mission| mission.status()),
                TreeStatus::Cancelled,
                "the stop command halted the running mission locally"
            );
            assert!(
                pilot.cancel_ticket.is_some(),
                "the stop issued one correlated remote cancellation"
            );
            // The remote cancellation's reply distinguishes its outcome; a
            // later stop with no running mission reports NotRunning without
            // another remote call.
            let idle_stop = pilot_command(
                "stop",
                5,
                Box::new(crate::runtime_authoring::pilot::BeginRequest {
                    job: 13,
                    duration_ms: 20,
                }),
            );
            pilot = pilot_step(&adapter, pilot, 60, 6, &idle_stop)?;
            assert!(
                pilot.cancel_ticket.is_some(),
                "the idle stop issued no second remote call"
            );
            Ok(())
        }

        /// Cancelling a running custom action runs its halt hook and leaves
        /// the tree terminal.
        #[test]
        fn cancelling_a_custom_action_runs_its_halt() -> phoxal::Result<()> {
            let _guard = crate::runtime_authoring::ACTION_HALT_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            crate::runtime_authoring::ACTION_HALT
                .store(false, std::sync::atomic::Ordering::Relaxed);
            let (adapter, mut pilot) = pilot_start(modes::ACTION_CANCEL)?;
            pilot = pilot_step(&adapter, pilot, 0, 0, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Running);
            // The step cancels the mission from this invocation on.
            pilot = pilot_step(&adapter, pilot, 10, 1, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Cancelled);
            assert!(
                crate::runtime_authoring::ACTION_HALT.load(std::sync::atomic::Ordering::Relaxed),
                "the halt hook ran when the branch was retired"
            );
            pilot = pilot_step(&adapter, pilot, 20, 2, &pilot_empty())?;
            assert_eq!(pilot.mission.status(), TreeStatus::Cancelled);
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Pilot: one behavior-tree runtime with a private call descriptor and two
// call fields, used for the focused local-time and completion-ownership
// semantics tests. Config selects the mission shape under test.
// ---------------------------------------------------------------------------

#[phoxal::messages(package = "phoxal.tests.authoring.pilot.v1")]
mod pilot {
    use phoxal::contracts::Queue;

    pub struct AskRequest {
        #[phoxal(tag = 1)]
        pub value: u64,
    }

    pub struct AskResponse {
        #[phoxal(tag = 1)]
        pub value: u64,
    }

    /// The retained mission report: what the owner path can observe of
    /// completion delivery without reading private state.
    pub struct PilotReport {
        #[phoxal(tag = 1)]
        pub primary_replies: u32,
        #[phoxal(tag = 2)]
        pub secondary_replies: u32,
        #[phoxal(tag = 3)]
        pub mission_phase: MissionPhase,
    }

    pub enum MissionPhase {
        Unspecified = 0,
        Running = 1,
        Succeeded = 2,
        Refused = 3,
        Failed = 4,
        Cancelled = 5,
        TimedOut = 6,
    }

    /// One captured queued event payload.
    pub struct ProbeEvent {
        #[phoxal(tag = 1)]
        pub job_id: u64,
    }

    /// One command-triggered mission start.
    pub struct BeginRequest {
        #[phoxal(tag = 1)]
        pub job: u64,
        #[phoxal(tag = 2)]
        pub duration_ms: u64,
    }

    /// The typed start outcome: Busy refuses rather than replacing.
    pub enum BeginAck {
        #[phoxal(tag = 1)]
        Accepted,
        #[phoxal(tag = 2)]
        Busy,
    }

    /// The typed stop outcome.
    pub enum StopAck {
        #[phoxal(tag = 1)]
        Stopped,
        #[phoxal(tag = 2)]
        NotRunning,
    }

    /// Two call fields declare the same private descriptor: each keeps its
    /// own completion ownership.
    #[phoxal::endpoints]
    pub struct PilotApi {
        #[phoxal::call]
        primary: super::PilotAsk,

        #[phoxal::call]
        secondary: super::PilotAsk,

        #[phoxal::input(max_items = 4, max_bytes = 256)]
        events: Queue<ProbeEvent>,

        #[phoxal::operation(
            contract = "phoxal.tests.authoring.pilot.v1.Begin",
            max_items = 4,
            max_bytes = 256
        )]
        begin: phoxal::contracts::RequestReply<BeginRequest, BeginAck>,

        #[phoxal::operation(
            contract = "phoxal.tests.authoring.pilot.v1.Stop",
            max_items = 4,
            max_bytes = 256
        )]
        stop: phoxal::contracts::RequestReply<BeginRequest, StopAck>,

        #[phoxal::output(max_items = 2)]
        report: Queue<PilotReport>,
    }
}

pub struct PilotAsk;

impl phoxal::contracts::Operation for PilotAsk {
    type Request = pilot::AskRequest;
    type Response = pilot::AskResponse;

    const METHOD: phoxal::contracts::CallMethod<Self::Request, Self::Response> =
        phoxal::contracts::CallMethod::new(
            "phoxal.tests.authoring.pilot.v1.Ask",
            "Ask",
            "ask",
            "phoxal.tests.authoring.pilot.v1.AskRequest",
            "phoxal.tests.authoring.pilot.v1.AskResponse",
            None,
            &[],
        );
}

use phoxal::runtime::behavior::{Sequence, Tree, TreeStatus};
use phoxal::runtime::input::CallCompletion;

/// Observes the custom action's halt hook from outside the runtime.
static ACTION_HALT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Serializes the two tests that share the ACTION_HALT flag: without it,
/// one test's reset can land between the other's halt hook and its read.
static ACTION_HALT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Compiled regressions from the round-6 review: shared ownership,
/// bounds, and terminal semantics of the behavior facility.
mod review_regressions {
    use super::pilot::AskRequest;
    use super::pilot::AskResponse;
    use super::pilot::pilot_api;
    use phoxal::contracts::ProstPayload;
    use phoxal::runtime::behavior::ActionOutcome;
    use phoxal::runtime::behavior::{
        FailureKind, Tree, TreeStatus, action, condition, guard, observe, repeat, selector,
        sequence, wait_event,
    };
    use phoxal::runtime::capture::{CaptureRegistry, InputDescriptor};
    use phoxal::runtime::input::{InputSnapshot, TransportInputSink};
    use phoxal::runtime::{Context, ExecutionDuration, ExecutionTime, StepContext, initialize};
    use std::sync::atomic::{AtomicUsize, Ordering};

    type PilotInputs = pilot_api::Inputs;

    fn inputs() -> PilotInputs {
        <PilotInputs as InputSnapshot>::empty()
    }

    fn context(millis: u64, index: u64) -> StepContext {
        StepContext::from_previous(
            ExecutionTime::from_nanos(millis * 1_000_000),
            ExecutionDuration::from_millis(10),
            Some(ExecutionTime::from_nanos(
                millis.saturating_sub(10) * 1_000_000,
            )),
            0,
            index,
        )
    }

    /// A hand-built context over one frozen cut, exactly like an authored
    /// step receives: no pending ledger and no capture registry unless a
    /// test names one.
    fn bare_context<'a>(
        step: &'a StepContext,
        inputs: &'a PilotInputs,
        outputs: &'a mut pilot_api::Outputs,
    ) -> Context<'a, super::Pilot> {
        Context::new(step, inputs, outputs)
    }

    /// A repeat whose children succeed immediately must still run one
    /// child per attempt, yield between attempts, and succeed only after
    /// the requested number of successes.
    #[test]
    fn repeat_immediate_success_runs_every_attempt() -> phoxal::Result<()> {
        let runs = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&runs);
        let mut tree = repeat::<super::Pilot, _>(3, move |_| {
            let counted = std::sync::Arc::clone(&counted);
            Ok(action(move |_ctx| {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(ActionOutcome::Succeeded)
            })
            .into_node())
        })
        .build()?;
        let inputs = inputs();
        for tick in 0..6 {
            let step = context(tick * 10, tick);
            let mut outputs = pilot_api::Outputs::default();
            let mut ctx = bare_context(&step, &inputs, &mut outputs);
            tree.tick(&mut ctx)?;
        }
        assert_eq!(
            runs.load(Ordering::SeqCst),
            3,
            "repeat stopped after the first immediately successful child"
        );
        assert_eq!(tree.status(), TreeStatus::Succeeded);
        Ok(())
    }

    /// A repeat of zero attempts succeeds without ever calling the
    /// factory, and a refusal from an attempt ends the repeat.
    #[test]
    fn repeat_zero_count_and_refusal_semantics() -> phoxal::Result<()> {
        let mut tree = repeat::<super::Pilot, _>(0, |_| {
            Err(phoxal::anyhow!(
                "the factory must not run for zero attempts"
            ))
        })
        .build()?;
        let step = context(0, 0);
        let inputs = inputs();
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = bare_context(&step, &inputs, &mut outputs);
        tree.tick(&mut ctx)?;
        assert_eq!(tree.status(), TreeStatus::Succeeded);

        let mut tree = repeat::<super::Pilot, _>(2, |_| Ok(condition(|_| false))).build()?;
        let mut ctx = bare_context(&step, &inputs, &mut outputs);
        tree.tick(&mut ctx)?;
        assert_eq!(
            tree.status(),
            TreeStatus::Refused,
            "a refusing attempt ends the repeat rather than retrying"
        );
        Ok(())
    }

    /// Composing a sequence with `into_node` preserves its declared
    /// deadline: composing can never silently drop the wrapper that
    /// `build` would apply.
    #[test]
    fn composing_a_sequence_preserves_its_deadline() -> phoxal::Result<()> {
        let mut tree = super::Sequence::<super::Pilot>::new()
            .delay(std::time::Duration::from_secs(1))
            .within(std::time::Duration::ZERO)
            .into_node()
            .build()?;
        let step = context(0, 0);
        let inputs = inputs();
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = bare_context(&step, &inputs, &mut outputs);
        tree.tick(&mut ctx)?;
        assert_eq!(tree.status(), TreeStatus::TimedOut);
        Ok(())
    }

    /// A dynamic repeat child deeper than the aggregate depth bound is
    /// refused before activation: the admission validator carries the
    /// insertion site's ancestor depth.
    #[test]
    fn dynamic_repeat_enforces_aggregate_depth_before_activation() -> phoxal::Result<()> {
        let mut tree = repeat::<super::Pilot, _>(1, |_| {
            let mut node = condition(|_| true);
            for _ in 0..phoxal::runtime::behavior::MAX_DEPTH + 1 {
                node = guard(|_| true, node);
            }
            Ok(node)
        })
        .build()?;
        let step = context(0, 0);
        let inputs = inputs();
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = bare_context(&step, &inputs, &mut outputs);
        assert!(
            tree.tick(&mut ctx).is_err(),
            "a dynamic child beyond MAX_DEPTH must be refused before activation"
        );
        Ok(())
    }

    /// A terminal tree releases its branch resources exactly once: a
    /// capture activated by an earlier successful child does not outlive
    /// the terminal refusal, and cancelling a terminal tree does nothing.
    #[test]
    fn terminal_failure_releases_observed_capture() -> phoxal::Result<()> {
        let captures = CaptureRegistry::default();
        let (observe_node, _handle) =
            observe::<super::Pilot, _>(&InputDescriptor::<AskResponse>::new("events"));
        let mut tree = sequence([observe_node, condition(|_| false)]).build()?;
        let step = context(0, 0);
        let inputs = inputs();
        let mut outputs = pilot_api::Outputs::default();
        {
            let mut ctx = bare_context(&step, &inputs, &mut outputs).with_captures(&captures);
            tree.tick(&mut ctx)?;
            tree.cancel(&mut ctx)?;
        }
        assert_eq!(tree.status(), TreeStatus::Refused);
        assert!(
            !captures.has_active("events"),
            "a terminal branch must release its capture without manual cleanup"
        );
        Ok(())
    }

    /// The retained terminal diagnostics expose the typed failure
    /// classification instead of relying on cause strings alone.
    #[test]
    fn terminal_kind_is_retained_for_diagnostics() -> phoxal::Result<()> {
        let mut tree = sequence([condition(|_: &Context<'_, super::Pilot>| false)]).build()?;
        let step = context(0, 0);
        let inputs = inputs();
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = bare_context(&step, &inputs, &mut outputs);
        tree.tick(&mut ctx)?;
        assert_eq!(tree.status(), TreeStatus::Refused);
        assert_eq!(tree.ended_kind(), Some(FailureKind::DomainRefusal));
        Ok(())
    }

    /// Capture overflow is authoritative: once items were dropped at the
    /// bound, a matching retained item can never turn the wait into
    /// success, and the loss surfaces as a typed failure.
    #[test]
    fn capture_overflow_is_visible_before_a_matching_retained_event() -> phoxal::Result<()> {
        let captures = CaptureRegistry::default();
        let (observe_node, handle) =
            observe::<super::Pilot, _>(&InputDescriptor::<AskResponse>::new("events"));
        let mut tree =
            sequence([observe_node, wait_event(handle, |_: &AskResponse| true)]).build()?;
        let step = context(0, 0);
        let inputs = inputs();
        let mut outputs = pilot_api::Outputs::default();
        {
            let mut ctx = bare_context(&step, &inputs, &mut outputs).with_captures(&captures);
            tree.tick(&mut ctx)?;
        }
        let payload = AskResponse { value: 7 }.encode_payload()?;
        for _ in 0..17 {
            captures.copy_admitted("events", 1, &payload);
        }
        let step = context(10, 1);
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = bare_context(&step, &inputs, &mut outputs).with_captures(&captures);
        tree.tick(&mut ctx)?;
        assert_eq!(
            tree.status(),
            TreeStatus::Failed,
            "a matching retained item must not hide the capture's loss"
        );
        Ok(())
    }

    /// A guard that stops holding after its child staged a call does not
    /// authorize selector fallback: the abandoned, unresolved remote
    /// effect is uncertain evidence, not a domain-safe refusal.
    #[test]
    fn guard_halt_after_a_submitted_call_does_not_authorize_fallback() -> phoxal::Result<()> {
        let runs = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&runs);
        let guarded = guard(
            |ctx: &Context<'_, super::Pilot>| ctx.invocation_index() == 0,
            super::Sequence::<super::Pilot>::new()
                .call(pilot_api::calls::primary(AskRequest { value: 1 }))
                .expect_response(|_: &AskResponse| true)
                .into_node(),
        );
        let fallback = action(move |_ctx| {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(ActionOutcome::Succeeded)
        })
        .into_node();
        let mut tree = selector([guarded, fallback]).build()?;
        let inputs = inputs();
        for tick in 0..2 {
            let step = context(tick * 10, tick);
            let mut outputs = pilot_api::Outputs::default();
            let mut ctx = bare_context(&step, &inputs, &mut outputs);
            tree.tick(&mut ctx)?;
        }
        assert_eq!(
            runs.load(Ordering::SeqCst),
            0,
            "an abandoned outstanding call is not a fallback-eligible refusal"
        );
        assert_eq!(tree.status(), TreeStatus::Refused);
        assert_eq!(tree.ended_kind(), Some(FailureKind::UncertainEffect));
        Ok(())
    }

    /// The same abandonment rule holds when the guard's child completed
    /// before the halt: a known remote outcome stays a domain-safe
    /// refusal, so the selector may fall back.
    #[test]
    fn guard_halt_after_a_completed_call_stays_fallback_eligible() -> phoxal::Result<()> {
        let guarded = guard(
            |ctx: &Context<'_, super::Pilot>| ctx.invocation_index() < 2,
            super::Sequence::<super::Pilot>::new()
                .call(pilot_api::calls::primary(AskRequest { value: 1 }))
                .expect_response(|_: &AskResponse| false)
                .into_node(),
        );
        let fallback = condition(|_| true);
        let mut tree = selector([guarded, fallback]).build()?;
        let inputs = inputs();

        // Invocation 0 stages the call; invocation 1 delivers a response
        // the predicate rejects: a known domain refusal while the guard
        // still holds, so the selector advances.
        let step = context(0, 0);
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = bare_context(&step, &inputs, &mut outputs);
        tree.tick(&mut ctx)?;
        let ticket = phoxal::runtime::outputs::compose_call_ticket(0, 0, 0)
            .expect("the probe's ticket space is representable");
        let step = context(10, 1);
        let mut completion_inputs = <PilotInputs as InputSnapshot>::empty();
        completion_inputs.set_call_completions(vec![
            phoxal::runtime::input::TransportCallCompletion {
                ticket,
                result: Ok(AskResponse { value: 1 }.encode_payload()?),
            },
        ])?;
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = bare_context(&step, &completion_inputs, &mut outputs);
        tree.tick(&mut ctx)?;
        assert_eq!(
            tree.status(),
            TreeStatus::Succeeded,
            "a known remote refusal stays fallback-eligible"
        );
        Ok(())
    }

    /// A minimal contract whose only endpoint is the queued input the
    /// capture watches.
    #[phoxal::messages(package = "phoxal.tests.authoring.capture_reset.v1")]
    mod reset_vocab {
        use phoxal::contracts::Queue;

        pub struct Sample {
            #[phoxal(tag = 1)]
            pub value: u64,
        }

        #[phoxal::endpoints]
        pub struct ResetApi {
            #[phoxal::input(max_items = 16, max_bytes = 1_024)]
            events: Queue<Sample>,
        }
    }

    /// A generated adapter owns its capture registry for the whole
    /// execution: reinitializing through the adapter releases every old
    /// capture slot, so repeated reset cycles can never exhaust the
    /// bounded capacity.
    pub(crate) struct CaptureResetBrain {
        tree: Tree<CaptureResetBrain>,
    }

    #[phoxal::runtime(contract = reset_vocab::ResetApi, period_ms = 10)]
    impl CaptureResetBrain {
        #[init]
        fn new(_config: ()) -> phoxal::Result<Self> {
            let (observe_node, handle) =
                observe::<Self, _>(&InputDescriptor::<reset_vocab::Sample>::new("events"));
            Ok(Self {
                tree: sequence([
                    observe_node,
                    wait_event(handle, |_: &reset_vocab::Sample| false),
                ])
                .build()?,
            })
        }

        #[step]
        fn advance(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
            self.tree.tick(ctx)
        }
    }

    #[test]
    fn adapter_reset_releases_old_capture_slots() -> phoxal::Result<()> {
        let adapter = phoxal_runtime_capture_reset_brain::Adapter::new();
        let inputs = <reset_vocab::reset_api::Inputs as InputSnapshot>::empty();
        for _cycle in 0..10 {
            let state = initialize(&adapter, ExecutionTime::default(), ())?;
            let step = context(0, 0);
            let (_state, _outputs) =
                crate::runtime_authoring::accept_candidate(&adapter, &step, state, &inputs)?;
            assert!(
                adapter.authoring.resources().captures.has_active("events"),
                "the cycle's tree activated its capture"
            );
            initialize(&adapter, ExecutionTime::default(), ())?;
            assert!(
                !adapter.authoring.resources().captures.has_active("events"),
                "reinitialization must release the prior execution's captures"
            );
        }
        Ok(())
    }
}

/// Mission shapes under test, selected by the runtime's configuration.
mod modes {
    /// `delay(100ms)` with no other stages.
    pub const DELAY: u64 = 1;
    /// `within(100ms)` around a predicate that never holds.
    pub const DEADLINE: u64 = 2;
    /// A tree-owned call on `secondary` whose response must satisfy the
    /// predicate.
    pub const TREE_CALL: u64 = 3;
    /// A tree-owned call on `secondary` beside one direct call on
    /// `primary` staged by the step.
    pub const CONCURRENT: u64 = 4;
    /// A tree-owned call on `secondary` that the step cancels from its
    /// third invocation on.
    pub const CANCEL: u64 = 5;
    /// A selector whose first branch fails its response predicate and
    /// whose second branch waits for a later invocation.
    pub const SELECTOR_FALLBACK: u64 = 6;
    /// A selector whose first branch ends with an uncertain remote
    /// outcome: the tree stops instead of trying the next branch.
    pub const SELECTOR_UNCERTAIN: u64 = 7;
    /// A guard that stops holding after its first invocation while its
    /// call child is still awaiting its reply.
    pub const GUARD_HALT: u64 = 8;
    /// Cooperative parallel waits of different lengths.
    pub const ALL_PARALLEL: u64 = 9;
    /// A race between a never-true wait and a delay.
    pub const RACE: u64 = 10;
    /// A typed continuation built from one decoded response.
    pub const CONTINUATION: u64 = 11;
    /// Three explicit repeated attempts with per-attempt responses.
    pub const REPEAT: u64 = 12;
    /// One custom action with an action-local halt hook.
    pub const ACTION: u64 = 13;
    /// The same custom action, cancelled by the step from its second
    /// invocation.
    pub const ACTION_CANCEL: u64 = 14;
    /// A capture activated before a call, with the awaited event arriving
    /// after the call's reply.
    pub const CAPTURE_AFTER_REPLY: u64 = 15;
    /// A capture whose event arrived before the reply: the wait retains
    /// it across the reply's invocation.
    pub const CAPTURE_BEFORE_REPLY: u64 = 16;
}

pub(crate) struct Pilot {
    mission: Tree<Pilot>,
    tick_mission: bool,
    cancel_from: Option<u64>,
    stage_direct: bool,
    direct_staged: bool,
    primary_replies: u32,
    secondary_replies: u32,
    tree_seen: Option<u64>,
    completions: u32,
    event_count: u32,
    cancel_on_event: bool,
    cancel_on_completion: bool,
    commanded: Option<Tree<Pilot>>,
    cancel_ticket: Option<phoxal::runtime::outputs::CallTicket<pilot::AskResponse>>,
    cancel_outcome: Option<&'static str>,
}

impl Pilot {
    /// The commanded mission's shape: one call whose response carries the
    /// command's own domain identity, then a bounded wait.
    fn commanded_mission(job: u64, ctx: &Context<'_, Self>) -> Result<Tree<Pilot>> {
        let _ = ctx;
        phoxal::runtime::behavior::sequence([
            Sequence::<Self>::new()
                .call(crate::runtime_authoring::pilot::pilot_api::calls::primary(
                    pilot::AskRequest { value: job },
                ))
                .expect_response(move |response: &pilot::AskResponse| response.value == job)
                .into_node(),
            phoxal::runtime::behavior::delay(std::time::Duration::from_millis(20)),
        ])
        .build()
    }

    fn mission_for(mode: u64) -> Result<Tree<Pilot>> {
        use phoxal::runtime::behavior::{
            action, all, condition, delay, guard, race, repeat, selector, wait_until,
        };
        match mode {
            modes::DELAY => Sequence::<Self>::new()
                .delay(std::time::Duration::from_millis(100))
                .build(),
            modes::DEADLINE => Sequence::<Self>::new()
                .wait_until(|_| false)
                .within(std::time::Duration::from_millis(100))
                .build(),
            modes::SELECTOR_FALLBACK => selector([
                // The predicate fails on the delivered response: an
                // expected domain refusal, eligible for the next branch.
                Sequence::<Self>::new()
                    .call(crate::runtime_authoring::pilot::pilot_api::calls::primary(
                        pilot::AskRequest { value: 1 },
                    ))
                    .expect_response(|_: &pilot::AskResponse| false)
                    .into_node(),
                condition(|_| false),
                wait_until(|ctx: &phoxal::runtime::Context<'_, Self>| ctx.invocation_index() >= 2),
            ])
            .build(),
            modes::SELECTOR_UNCERTAIN => selector([
                Sequence::<Self>::new()
                    .call(crate::runtime_authoring::pilot::pilot_api::calls::primary(
                        pilot::AskRequest { value: 1 },
                    ))
                    .expect_response(|_: &pilot::AskResponse| true)
                    .into_node(),
                // The next branch would succeed immediately; the selector
                // must never reach it after an uncertain outcome.
                condition(|_| true),
            ])
            .build(),
            modes::GUARD_HALT => guard(
                |ctx| ctx.invocation_index() < 1,
                Sequence::<Self>::new()
                    .call(crate::runtime_authoring::pilot::pilot_api::calls::primary(
                        pilot::AskRequest { value: 1 },
                    ))
                    .expect_response(|response: &pilot::AskResponse| response.value == 1)
                    .into_node(),
            )
            .build(),
            modes::ALL_PARALLEL => all([
                wait_until(|ctx: &phoxal::runtime::Context<'_, Self>| ctx.invocation_index() >= 1),
                wait_until(|ctx: &phoxal::runtime::Context<'_, Self>| ctx.invocation_index() >= 3),
            ])
            .build(),
            modes::RACE => race([
                wait_until(|_| false),
                delay(std::time::Duration::from_millis(100)),
            ])
            .build(),
            modes::CONTINUATION => Sequence::<Self>::new()
                .call(crate::runtime_authoring::pilot::pilot_api::calls::primary(
                    pilot::AskRequest { value: 5 },
                ))
                .then(|response: pilot::AskResponse| {
                    // The continuation owns the decoded response: the
                    // built child consumes it by value.
                    let observed = response.value;
                    Ok(condition(move |_| observed == 5))
                })
                .build(),
            modes::REPEAT => repeat::<Self, _>(3, |attempt| {
                Ok(Sequence::<Self>::new()
                    .call(crate::runtime_authoring::pilot::pilot_api::calls::primary(
                        pilot::AskRequest {
                            value: u64::from(attempt),
                        },
                    ))
                    .expect_response(move |reply: &pilot::AskResponse| {
                        reply.value == u64::from(attempt)
                    })
                    .into_node())
            })
            .build(),
            modes::CAPTURE_AFTER_REPLY | modes::CAPTURE_BEFORE_REPLY => {
                let (capture, events) = phoxal::runtime::behavior::observe(
                    &crate::runtime_authoring::pilot::pilot_api::inputs::EVENTS,
                );
                phoxal::runtime::behavior::sequence([
                    capture,
                    Sequence::<Self>::new()
                        .call(crate::runtime_authoring::pilot::pilot_api::calls::primary(
                            pilot::AskRequest { value: 1 },
                        ))
                        .expect_response(|response: &pilot::AskResponse| response.value == 1)
                        .into_node(),
                    phoxal::runtime::behavior::wait_event(events, |event: &pilot::ProbeEvent| {
                        event.job_id == 7
                    }),
                ])
                .build()
            }
            modes::ACTION | modes::ACTION_CANCEL => {
                action(|ctx: &mut phoxal::runtime::Context<'_, Self>| {
                    Ok(if ctx.invocation_index() >= 2 {
                        phoxal::runtime::behavior::ActionOutcome::Succeeded
                    } else {
                        phoxal::runtime::behavior::ActionOutcome::Running
                    })
                })
                .with_halt(|_| {
                    crate::runtime_authoring::ACTION_HALT
                        .store(true, std::sync::atomic::Ordering::Relaxed)
                })
                .into_node()
                .build()
            }
            _ => Sequence::<Self>::new()
                .call(
                    crate::runtime_authoring::pilot::pilot_api::calls::secondary(
                        pilot::AskRequest { value: 1 },
                    ),
                )
                .expect_response(|response: &pilot::AskResponse| response.value == 1)
                .build(),
        }
    }
}

#[phoxal::runtime(contract = crate::runtime_authoring::pilot::PilotApi, period_ms = 10)]
impl Pilot {
    #[init]
    fn new(mode: u64) -> Result<Self> {
        Ok(Self {
            mission: Self::mission_for(mode)?,
            tick_mission: true,
            cancel_from: if mode == modes::CANCEL {
                Some(2)
            } else if mode == modes::ACTION_CANCEL {
                Some(1)
            } else {
                None
            },
            stage_direct: mode == modes::CONCURRENT,
            direct_staged: false,
            primary_replies: 0,
            secondary_replies: 0,
            tree_seen: None,
            completions: 0,
            event_count: 0,
            cancel_on_event: false,
            cancel_on_completion: false,
            commanded: None,
            cancel_ticket: None,
            cancel_outcome: None,
        })
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> Result<()> {
        if let Some(from) = self.cancel_from
            && ctx.invocation_index() >= from
            && self.mission.status() == TreeStatus::Running
        {
            self.mission.cancel(ctx)?;
        }
        let before = self.mission.status();
        if self.tick_mission {
            self.mission.tick(ctx)?;
        }
        if let Some(commanded) = self.commanded.as_mut() {
            commanded.tick(ctx)?;
        }
        if self.stage_direct && !self.direct_staged {
            // The direct field method records this field's exclusive
            // ownership of the eventual completion.
            ctx.primary(pilot::AskRequest { value: 7 })?;
            self.direct_staged = true;
        }
        // Effects fire on the transition into the terminal state, proving
        // the latch: later ticks of the same terminal tree add nothing.
        if before == TreeStatus::Running && self.mission.status() == TreeStatus::Succeeded {
            self.completions = self.completions.saturating_add(1);
        }
        // The owner path observes delivery through this retained event
        // batch; private state stays private.
        ctx.emit_report(self.report())?;
        Ok(())
    }

    #[complete(primary)]
    fn primary_completed(
        &mut self,
        ctx: &mut Context<'_, Self>,
        _completion: CallCompletion<pilot::AskResponse>,
    ) -> Result<()> {
        self.primary_replies = self.primary_replies.saturating_add(1);
        if self.cancel_on_completion
            && let Some(commanded) = self.commanded.as_mut()
        {
            commanded.cancel(ctx)?;
        }
        Ok(())
    }

    fn report(&self) -> pilot::PilotReport {
        use phoxal::runtime::behavior::TreeStatus as Status;
        let mission_phase = match self.mission.status() {
            Status::Running => pilot::MissionPhase::Running,
            Status::Succeeded => pilot::MissionPhase::Succeeded,
            Status::Refused => pilot::MissionPhase::Refused,
            Status::Failed => pilot::MissionPhase::Failed,
            Status::Cancelled => pilot::MissionPhase::Cancelled,
            Status::TimedOut => pilot::MissionPhase::TimedOut,
        };
        pilot::PilotReport {
            primary_replies: self.primary_replies,
            secondary_replies: self.secondary_replies,
            mission_phase,
        }
    }

    #[complete(secondary)]
    fn secondary_completed(
        &mut self,
        _ctx: &mut Context<'_, Self>,
        completion: CallCompletion<pilot::AskResponse>,
    ) -> Result<()> {
        self.secondary_replies = self.secondary_replies.saturating_add(1);
        // A pending cancellation correlates by its ticket: the remote
        // outcome distinguishes Cancelled from UnknownJob and transport
        // uncertainty stays visible rather than assumed stopped.
        if self
            .cancel_ticket
            .as_ref()
            .is_some_and(|pending| pending.id() == completion.ticket())
        {
            self.cancel_ticket = None;
            self.cancel_outcome = match completion.into_result() {
                Ok(response) if response.value == 1 => Some("cancelled"),
                Ok(_) => Some("unknown job"),
                Err(_) => Some("uncertain"),
            };
        }
        Ok(())
    }

    /// The command-triggered mission start: Busy refuses rather than
    /// silently replacing a running mission; a terminal mission is
    /// replaced by a fresh one carrying the command's domain identity.
    #[handle(begin)]
    fn begin(
        &mut self,
        ctx: &mut Context<'_, Self>,
        request: pilot::BeginRequest,
    ) -> Result<pilot::BeginAck> {
        if self
            .commanded
            .as_ref()
            .is_some_and(|mission| mission.status() == TreeStatus::Running)
        {
            return Ok(pilot::BeginAck::Busy);
        }
        self.commanded = Some(Self::commanded_mission(request.job, ctx)?);
        Ok(pilot::BeginAck::Accepted)
    }

    /// The command-triggered stop: local halt first, then one explicit
    /// remote cancellation whose outcome the completion handler observes.
    #[handle(stop)]
    fn stop(
        &mut self,
        ctx: &mut Context<'_, Self>,
        request: pilot::BeginRequest,
    ) -> Result<pilot::StopAck> {
        let Some(commanded) = self.commanded.as_mut() else {
            return Ok(pilot::StopAck::NotRunning);
        };
        if commanded.status() != TreeStatus::Running {
            return Ok(pilot::StopAck::NotRunning);
        }
        commanded.cancel(ctx)?;
        // The remote cancellation is its own typed call; neither the
        // local halt nor an unacknowledged request proves the remote
        // stopped.
        self.cancel_ticket = Some(ctx.secondary(pilot::AskRequest { value: request.job })?);
        Ok(pilot::StopAck::Stopped)
    }

    #[handle(events)]
    fn on_event(&mut self, ctx: &mut Context<'_, Self>, _event: pilot::ProbeEvent) -> Result<()> {
        // The ordinary handler and an explicit capture may both observe
        // the same event; this counter proves the handler still ran.
        self.event_count = self.event_count.saturating_add(1);
        if self.cancel_on_event
            && let Some(commanded) = self.commanded.as_mut()
        {
            commanded.cancel(ctx)?;
        }
        Ok(())
    }
}

/// Compiled regressions from the round-7 review: scope-owned resource
/// lifetimes across ordinary public compositions, invocation-identity
/// gating, exact dynamic-allowance reclamation, and bounded capacity.
mod review7_regressions {
    use super::pilot::AskResponse;
    use super::pilot::pilot_api;
    use phoxal::contracts::ProstPayload;
    use phoxal::runtime::behavior::{
        Sequence, Tree, TreeStatus, all, condition, guard, observe, race, repeat, selector,
        sequence, wait_event,
    };
    use phoxal::runtime::capture::{CaptureHandle, CaptureRegistry, InputDescriptor};
    use phoxal::runtime::input::{InputSnapshot, TransportCallCompletion, TransportInputSink};
    use phoxal::runtime::outputs::compose_call_ticket;
    use phoxal::runtime::{Context, ExecutionDuration, ExecutionTime, StepContext, initialize};
    use std::sync::atomic::{AtomicUsize, Ordering};

    type PilotInputs = pilot_api::Inputs;

    fn empty() -> PilotInputs {
        <PilotInputs as InputSnapshot>::empty()
    }

    fn context(millis: u64, index: u64) -> StepContext {
        if index == 0 {
            StepContext::first(
                ExecutionTime::from_nanos(millis * 1_000_000),
                ExecutionDuration::from_millis(10),
            )
        } else {
            StepContext::from_previous(
                ExecutionTime::from_nanos(millis * 1_000_000),
                ExecutionDuration::from_millis(10),
                Some(ExecutionTime::from_nanos(
                    millis.saturating_sub(10) * 1_000_000,
                )),
                0,
                index,
            )
        }
    }

    fn tick_once(
        tree: &mut Tree<super::Pilot>,
        captures: Option<&CaptureRegistry>,
        millis: u64,
        index: u64,
        inputs: &PilotInputs,
    ) -> phoxal::Result<()> {
        let step = context(millis, index);
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = Context::<super::Pilot>::new(&step, inputs, &mut outputs);
        if let Some(captures) = captures {
            ctx = ctx.with_captures(captures);
        }
        tree.tick(&mut ctx)
    }

    fn capture_of() -> (
        phoxal::runtime::behavior::Node<super::Pilot>,
        CaptureHandle<AskResponse>,
    ) {
        observe::<super::Pilot, _>(&InputDescriptor::<AskResponse>::new("events"))
    }

    /// A terminal all releases the capture its successful child
    /// activated: a done flag records completion, never cleanup.
    #[test]
    fn all_success_releases_capture() -> phoxal::Result<()> {
        let captures = CaptureRegistry::default();
        let (observe_node, _handle) = capture_of();
        let mut tree = all([observe_node]).build()?;
        tick_once(&mut tree, Some(&captures), 0, 0, &empty())?;
        assert_eq!(tree.status(), TreeStatus::Succeeded);
        assert!(
            !captures.has_active("events"),
            "the terminal all retired its done child's capture"
        );
        Ok(())
    }

    /// A race winner's capture releases at the scope's terminal state.
    #[test]
    fn race_winner_releases_capture() -> phoxal::Result<()> {
        let captures = CaptureRegistry::default();
        let (observe_node, _handle) = capture_of();
        let mut tree = race([observe_node]).build()?;
        tick_once(&mut tree, Some(&captures), 0, 0, &empty())?;
        assert_eq!(tree.status(), TreeStatus::Succeeded);
        assert!(!captures.has_active("events"));
        Ok(())
    }

    /// A guard whose subtree refused still owns that subtree's
    /// resources: the halted flag gates re-ticking, never cleanup.
    #[test]
    fn guard_refusal_releases_capture() -> phoxal::Result<()> {
        let captures = CaptureRegistry::default();
        let (observe_node, _handle) = capture_of();
        let inner = sequence([
            observe_node,
            condition(|_: &Context<'_, super::Pilot>| false),
        ]);
        let mut tree = guard(|_: &Context<'_, super::Pilot>| true, inner).build()?;
        tick_once(&mut tree, Some(&captures), 0, 0, &empty())?;
        assert_eq!(tree.status(), TreeStatus::Refused);
        assert!(!captures.has_active("events"));
        Ok(())
    }

    /// A selector retires the failed branch's resources before entering
    /// its replacement: the branch capture cannot leak across the
    /// fallback's lifetime while the tree keeps running.
    #[test]
    fn selector_retires_failed_branch_before_waiting() -> phoxal::Result<()> {
        let captures = CaptureRegistry::default();
        let (observe_node, _handle) = capture_of();
        let failed = sequence([
            observe_node,
            condition(|_: &Context<'_, super::Pilot>| false),
        ]);
        let fallback = phoxal::runtime::behavior::wait_until(|_| false);
        let mut tree = selector([failed, fallback]).build()?;
        tick_once(&mut tree, Some(&captures), 0, 0, &empty())?;
        assert_eq!(tree.status(), TreeStatus::Running, "the fallback waits");
        assert!(
            !captures.has_active("events"),
            "the abandoned branch's capture retired before the fallback ran"
        );
        Ok(())
    }

    /// The intentional surviving-capture lifetime: an observe beneath an
    /// all keeps its capture active after its own success, because the
    /// consuming wait in the SAME all still needs it.
    #[test]
    fn observe_survives_its_success_for_a_later_wait_in_the_same_all() -> phoxal::Result<()> {
        let captures = CaptureRegistry::default();
        let (observe_node, handle) = capture_of();
        let mut tree = all([
            observe_node,
            wait_event(handle, |event: &AskResponse| event.value == 7),
        ])
        .build()?;
        tick_once(&mut tree, Some(&captures), 0, 0, &empty())?;
        assert_eq!(tree.status(), TreeStatus::Running);
        assert!(
            captures.has_active("events"),
            "the successful observe's capture survives for its consuming wait"
        );
        captures.copy_admitted("events", 1, &AskResponse { value: 7 }.encode_payload()?);
        tick_once(&mut tree, Some(&captures), 10, 1, &empty())?;
        assert_eq!(tree.status(), TreeStatus::Succeeded);
        assert!(!captures.has_active("events"));
        Ok(())
    }

    /// The same surviving lifetime across scopes: the all completed and
    /// the enclosing sequence's later wait still consumes the capture.
    #[test]
    fn observe_survives_a_completed_all_for_a_later_wait_in_the_enclosing_sequence()
    -> phoxal::Result<()> {
        let captures = CaptureRegistry::default();
        let (observe_node, handle) = capture_of();
        let mut tree = sequence([
            all([observe_node]),
            wait_event(handle, |event: &AskResponse| event.value == 7),
        ])
        .build()?;
        tick_once(&mut tree, Some(&captures), 0, 0, &empty())?;
        assert_eq!(tree.status(), TreeStatus::Running);
        assert!(
            captures.has_active("events"),
            "the completed all's capture survives for the enclosing wait"
        );
        captures.copy_admitted("events", 1, &AskResponse { value: 7 }.encode_payload()?);
        tick_once(&mut tree, Some(&captures), 10, 1, &empty())?;
        assert_eq!(tree.status(), TreeStatus::Succeeded);
        assert!(!captures.has_active("events"));
        Ok(())
    }

    /// Two ticks of the same invocation — the same context or a fresh
    /// one — never start two repeat attempts; the next attempt waits
    /// for the next invocation.
    #[test]
    fn repeat_attempts_separate_on_invocation_identity() -> phoxal::Result<()> {
        let runs = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&runs);
        let mut tree = repeat::<super::Pilot, _>(3, move |_| {
            let counted = std::sync::Arc::clone(&counted);
            Ok(phoxal::runtime::behavior::action(move |_| {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(phoxal::runtime::behavior::ActionOutcome::Succeeded)
            })
            .into_node())
        })
        .build()?;
        let inputs = empty();

        // Two ticks through the SAME context: one attempt.
        let step = context(0, 0);
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = Context::<super::Pilot>::new(&step, &inputs, &mut outputs);
        tree.tick(&mut ctx)?;
        tree.tick(&mut ctx)?;
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "same invocation, one attempt"
        );

        // A fresh context that still names the SAME invocation: still one.
        tick_once(&mut tree, None, 0, 0, &inputs)?;
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "a second context of the same invocation starts no attempt"
        );

        // The next invocation admits the next attempt.
        tick_once(&mut tree, None, 10, 1, &inputs)?;
        assert_eq!(runs.load(Ordering::SeqCst), 2, "the next invocation admits");
        Ok(())
    }

    /// Ten sequential missions, each observe-and-wait, complete through
    /// one registry: released captures never exhaust the eight-slot
    /// bound.
    #[test]
    fn repeated_missions_never_exhaust_the_capture_bound() -> phoxal::Result<()> {
        let captures = CaptureRegistry::default();
        for mission in 0..10_u64 {
            let (observe_node, handle) = capture_of();
            let wanted = mission;
            let mut tree = sequence([
                observe_node,
                wait_event(handle, move |event: &AskResponse| event.value == wanted),
            ])
            .build()?;
            tick_once(
                &mut tree,
                Some(&captures),
                mission * 10,
                mission * 2,
                &empty(),
            )?;
            captures.copy_admitted(
                "events",
                mission * 2 + 1,
                &AskResponse { value: mission }.encode_payload()?,
            );
            tick_once(
                &mut tree,
                Some(&captures),
                mission * 10 + 10,
                mission * 2 + 1,
                &empty(),
            )?;
            assert_eq!(
                tree.status(),
                TreeStatus::Succeeded,
                "mission {mission} completed"
            );
            assert!(
                !captures.has_active("events"),
                "mission {mission} released its capture"
            );
        }
        Ok(())
    }

    /// One dedicated calling contract for the capacity and continuation
    /// rounds: a call endpoint the rounds' trees own end to end.
    #[phoxal::messages(package = "phoxal.tests.authoring.rounds.v1")]
    pub(crate) mod rounds_vocab {
        use phoxal::contracts::RequestReply;

        pub struct Ask {
            #[phoxal(tag = 1)]
            pub attempt: u64,
        }

        pub struct Reply {
            #[phoxal(tag = 1)]
            pub attempt: u64,
        }

        #[phoxal::endpoints]
        pub struct RoundsApi {
            #[phoxal::call(
                contract = "phoxal.tests.authoring.rounds.v1.Ask",
                max_items = 8,
                max_bytes = 1_024
            )]
            ask: RequestReply<Ask, Reply>,
        }
    }

    /// Rounds of call-owning trees: each round stages its call, the next
    /// invocation delivers the correlated reply, and a typed continuation
    /// beneath a completed all consumes it.
    pub(crate) struct RoundsBrain {
        tree: Tree<RoundsBrain>,
    }

    fn rounds_tree(rounds: u32, refusing: bool) -> phoxal::Result<Tree<RoundsBrain>> {
        repeat::<RoundsBrain, _>(rounds, move |attempt| {
            let round = u64::from(attempt);
            let refuse = refusing;
            Ok(Sequence::<RoundsBrain>::new()
                .call(rounds_vocab::rounds_api::calls::ask(rounds_vocab::Ask {
                    attempt: round,
                }))
                .then(move |reply: rounds_vocab::Reply| {
                    let observed = reply.attempt;
                    Ok(all([condition(move |_| !refuse && observed == round)]))
                })
                .into_node())
        })
        .build()
    }

    #[phoxal::runtime(contract = rounds_vocab::RoundsApi, period_ms = 10)]
    impl RoundsBrain {
        #[init]
        fn new(_config: ()) -> phoxal::Result<Self> {
            Ok(Self {
                tree: rounds_tree(300, false)?,
            })
        }

        #[step]
        fn advance(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
            self.tree.tick(ctx)
        }
    }

    /// Three hundred call rounds through the real generated adapter —
    /// beyond the 256-call pending capacity: each round's completion is
    /// consumed and its ledger slot released, and the typed continuation
    /// beneath the completed all reclaims its dynamic allowance exactly.
    /// A leak of either bound surfaces as an invocation fault, so
    /// reaching Succeeded proves exact reservation and reclamation.
    #[test]
    fn three_hundred_call_rounds_exceed_pending_capacity_and_reclaim_exactly() -> phoxal::Result<()>
    {
        let adapter = phoxal_runtime_rounds_brain::Adapter::new();
        let mut service =
            initialize(&adapter, ExecutionTime::default(), ()).expect("rounds brain initializes");
        let epoch = adapter.execution_epoch();
        for invocation in 0..700_u64 {
            let mut round_inputs = <rounds_vocab::rounds_api::Inputs as InputSnapshot>::empty();
            if invocation % 2 == 1 {
                // Deliver the reply for the call staged last invocation.
                let ticket = compose_call_ticket(epoch, invocation - 1, 0)
                    .expect("ticket space representable");
                round_inputs.set_call_completions(vec![TransportCallCompletion {
                    ticket,
                    result: Ok(rounds_vocab::Reply {
                        attempt: (invocation - 1) / 2,
                    }
                    .encode_payload()?),
                }])?;
            }
            let step = context(invocation * 10, invocation);
            let (next, _outputs) = crate::runtime_authoring::accept_candidate(
                &adapter,
                &step,
                service,
                &round_inputs,
            )?;
            if next.tree.status() != TreeStatus::Running {
                assert_eq!(
                    next.tree.status(),
                    TreeStatus::Succeeded,
                    "the rounds completed without exhausting a bound"
                );
                return Ok(());
            }
            service = next;
        }
        panic!("300 rounds did not complete within 700 invocations");
    }

    /// A refusing continuation beneath the same completed-all shape ends
    /// the repeat terminally instead of retrying.
    #[test]
    fn refusing_continuation_under_all_ends_the_repeat() -> phoxal::Result<()> {
        let mut tree = rounds_tree(3, true)?;
        let inputs = <rounds_vocab::rounds_api::Inputs as InputSnapshot>::empty();
        let step = context(0, 0);
        let mut outputs = rounds_vocab::rounds_api::Outputs::default();
        let mut ctx = Context::<RoundsBrain>::new(&step, &inputs, &mut outputs);
        tree.tick(&mut ctx)?;
        assert_eq!(
            tree.status(),
            TreeStatus::Running,
            "the first round's call is outstanding"
        );
        // Deliver a reply whose continuation condition refuses; a
        // hand-built context carries epoch zero.
        let ticket = compose_call_ticket(0, 0, 0).expect("ticket space representable");
        let mut completion_inputs = <rounds_vocab::rounds_api::Inputs as InputSnapshot>::empty();
        completion_inputs.set_call_completions(vec![TransportCallCompletion {
            ticket,
            result: Ok(rounds_vocab::Reply { attempt: 0 }.encode_payload()?),
        }])?;
        let step = context(10, 1);
        let mut outputs = rounds_vocab::rounds_api::Outputs::default();
        let mut ctx = Context::<RoundsBrain>::new(&step, &completion_inputs, &mut outputs);
        tree.tick(&mut ctx)?;
        assert_eq!(
            tree.status(),
            TreeStatus::Refused,
            "the refusing continuation ended the repeat rather than retrying"
        );
        Ok(())
    }
}

/// Compiled regressions from the round-8 review: exact admission
/// reservation ownership — the recorded charge returns whole exactly
/// once, independently of live-graph shrinkage, and nested dynamic
/// allocations hold separate charges that never double-credit.
mod review8_regressions {
    use super::pilot::pilot_api;
    use super::review7_regressions::rounds_vocab;
    use phoxal::contracts::ProstPayload;
    use phoxal::runtime::behavior::ActionOutcome;
    use phoxal::runtime::behavior::{
        Sequence, Tree, TreeStatus, action, all, condition, guard, race, repeat, selector,
        sequence, wait_until,
    };
    use phoxal::runtime::input::{InputSnapshot, TransportCallCompletion, TransportInputSink};
    use phoxal::runtime::outputs::compose_call_ticket;
    use phoxal::runtime::{Context, ExecutionDuration, ExecutionTime, StepContext, initialize};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type PilotInputs = pilot_api::Inputs;

    fn empty() -> PilotInputs {
        <PilotInputs as InputSnapshot>::empty()
    }

    fn context(millis: u64, index: u64) -> StepContext {
        if index == 0 {
            StepContext::first(
                ExecutionTime::from_nanos(millis * 1_000_000),
                ExecutionDuration::from_millis(10),
            )
        } else {
            StepContext::from_previous(
                ExecutionTime::from_nanos(millis * 1_000_000),
                ExecutionDuration::from_millis(10),
                Some(ExecutionTime::from_nanos(
                    millis.saturating_sub(10) * 1_000_000,
                )),
                0,
                index,
            )
        }
    }

    fn tick_pilot(tree: &mut Tree<super::Pilot>, millis: u64, index: u64) -> phoxal::Result<()> {
        let step = context(millis, index);
        let inputs = empty();
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = Context::<super::Pilot>::new(&step, &inputs, &mut outputs);
        tree.tick(&mut ctx)
    }

    /// The exact 1,100-round review case: every attempt's guard
    /// destructively removes its own child before the repeat releases
    /// the attempt, yet the recorded admission charge returns whole —
    /// capacity never leaks, so all 1,100 attempts complete.
    #[test]
    fn repeated_guard_fallback_reclaims_exact_admitted_size() -> phoxal::Result<()> {
        let mut tree = repeat::<super::Pilot, _>(1_100, |_| {
            Ok(selector([
                guard(
                    |_: &Context<'_, super::Pilot>| false,
                    condition(|_: &Context<'_, super::Pilot>| true),
                ),
                condition(|_: &Context<'_, super::Pilot>| true),
            ]))
        })
        .build()?;
        for tick in 0..1_200_u64 {
            tick_pilot(&mut tree, tick * 10, tick)
                .map_err(|error| phoxal::anyhow!("attempt {tick}: {error}"))?;
        }
        assert_eq!(tree.status(), TreeStatus::Succeeded);
        Ok(())
    }

    /// The exact over-limit review case: retiring nested repeats must
    /// return their separate recorded charges — never a recursively
    /// recomputed live size — so the inflated allowance cannot admit a
    /// 1,001-leaf fallback beyond MAX_NODES. Admission faults before the
    /// first action leaf executes.
    #[test]
    fn retiring_nested_repeats_must_not_inflate_node_allowance() -> phoxal::Result<()> {
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        let initial = guard(
            |ctx: &Context<'_, super::Pilot>| ctx.invocation_index() == 0,
            repeat(1, |_| {
                Ok(repeat(1, |_| {
                    let mut nodes = Vec::new();
                    for _ in 0..100 {
                        nodes.push(condition(|_: &Context<'_, super::Pilot>| true));
                    }
                    nodes.push(wait_until(|_| false));
                    Ok(sequence(nodes))
                }))
            }),
        );
        let fallback = repeat(1, move |_| {
            let mut nodes = Vec::new();
            for _ in 0..phoxal::runtime::behavior::MAX_NODES + 1 {
                let counted = Arc::clone(&counted);
                nodes.push(
                    action(move |_| {
                        counted.fetch_add(1, Ordering::SeqCst);
                        Ok(ActionOutcome::Succeeded)
                    })
                    .into_node(),
                );
            }
            Ok(sequence(nodes))
        });
        let mut tree = selector([initial, fallback]).build()?;
        tick_pilot(&mut tree, 0, 0)?;
        let fault = tick_pilot(&mut tree, 10, 1);
        assert!(
            fault.is_err(),
            "the oversized fallback must be refused at admission"
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            0,
            "no action leaf executed beyond the node bound"
        );
        Ok(())
    }

    /// Guard and within removals hold their static descendants'
    /// reservations until the enclosing dynamic scope ends: repeated
    /// expiring within scopes conserve capacity across 1,100 rounds.
    #[test]
    fn repeated_expiring_within_scopes_conserve_capacity() -> phoxal::Result<()> {
        let mut tree = repeat::<super::Pilot, _>(1_100, |_| {
            Ok(Sequence::<super::Pilot>::new()
                .delay(std::time::Duration::from_millis(0))
                .within(std::time::Duration::from_secs(1))
                .into_node())
        })
        .build()?;
        for tick in 0..1_200_u64 {
            tick_pilot(&mut tree, tick * 10, tick)?;
        }
        assert_eq!(tree.status(), TreeStatus::Succeeded);
        Ok(())
    }

    /// Repeated refusing scopes also conserve capacity: each refused
    /// attempt releases its recorded charge whole, so 1,100 refusals
    /// under a selector fallback complete without a capacity fault.
    #[test]
    fn repeated_refusing_scopes_conserve_capacity() -> phoxal::Result<()> {
        let mut tree = repeat::<super::Pilot, _>(1_100, |_| {
            Ok(selector([
                guard(|_: &Context<'_, super::Pilot>| false, wait_until(|_| false)),
                condition(|_: &Context<'_, super::Pilot>| true),
            ]))
        })
        .build()?;
        for tick in 0..1_200_u64 {
            tick_pilot(&mut tree, tick * 10, tick)?;
        }
        assert_eq!(tree.status(), TreeStatus::Succeeded);
        Ok(())
    }

    /// All/race winner and loser retirement with nested dynamic children:
    /// each nested repeat's charge returns exactly once through the walk,
    /// so 600 rounds of a winning race over nested repeats complete.
    #[test]
    fn race_winners_and_losers_with_nested_repeats_conserve() -> phoxal::Result<()> {
        let mut tree = repeat::<super::Pilot, _>(600, |_| {
            Ok(race([
                repeat(1, |_| {
                    Ok(sequence([condition(|_: &Context<'_, super::Pilot>| true)]))
                }),
                wait_until(|_| false),
            ]))
        })
        .build()?;
        for tick in 0..1_400_u64 {
            tick_pilot(&mut tree, tick * 10, tick)?;
        }
        assert_eq!(tree.status(), TreeStatus::Succeeded);
        Ok(())
    }

    /// The continuation-BENEATH-an-all shape, distinct from the
    /// continuation-producing-an-all shape of the 300-call fixture: an
    /// all branch owns a call whose typed continuation builds a child,
    /// the sibling waits for the invocation gate, and every round
    /// releases the continuation's recorded charge exactly. Fifty rounds
    /// over nested dynamic charges conserve capacity end to end.
    pub(crate) struct AllContinuationBrain {
        tree: Tree<AllContinuationBrain>,
    }

    fn all_continuation_tree(rounds: u32) -> phoxal::Result<Tree<AllContinuationBrain>> {
        repeat::<AllContinuationBrain, _>(rounds, move |attempt| {
            let round = u64::from(attempt);
            Ok(all([
                Sequence::<AllContinuationBrain>::new()
                    .call(rounds_vocab::rounds_api::calls::ask(rounds_vocab::Ask {
                        attempt: round,
                    }))
                    .then(move |reply: rounds_vocab::Reply| {
                        let observed = reply.attempt;
                        Ok(condition(move |_| observed == round))
                    })
                    .into_node(),
                wait_until(|ctx: &Context<'_, AllContinuationBrain>| ctx.invocation_index() > 0),
            ]))
        })
        .build()
    }

    #[phoxal::runtime(contract = rounds_vocab::RoundsApi, period_ms = 10)]
    impl AllContinuationBrain {
        #[init]
        fn new(_config: ()) -> phoxal::Result<Self> {
            Ok(Self {
                tree: all_continuation_tree(50)?,
            })
        }

        #[step]
        fn advance(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
            self.tree.tick(ctx)
        }
    }

    #[test]
    fn continuation_under_an_all_branch_conserve_exact_charges() -> phoxal::Result<()> {
        let adapter = phoxal_runtime_all_continuation_brain::Adapter::new();
        let mut service = initialize(&adapter, ExecutionTime::default(), ())
            .expect("all-continuation brain initializes");
        let epoch = adapter.execution_epoch();
        for invocation in 0..160_u64 {
            let mut round_inputs = <rounds_vocab::rounds_api::Inputs as InputSnapshot>::empty();
            if invocation % 2 == 1 {
                let ticket = compose_call_ticket(epoch, invocation - 1, 0)
                    .expect("ticket space representable");
                round_inputs.set_call_completions(vec![TransportCallCompletion {
                    ticket,
                    result: Ok(rounds_vocab::Reply {
                        attempt: (invocation - 1) / 2,
                    }
                    .encode_payload()?),
                }])?;
            }
            let step = context(invocation * 10, invocation);
            let (next, _outputs) = crate::runtime_authoring::accept_candidate(
                &adapter,
                &step,
                service,
                &round_inputs,
            )?;
            if next.tree.status() != TreeStatus::Running {
                assert_eq!(
                    next.tree.status(),
                    TreeStatus::Succeeded,
                    "50 continuation-under-all rounds conserved every charge"
                );
                return Ok(());
            }
            service = next;
        }
        panic!("50 rounds did not complete within 160 invocations");
    }
}

/// Compiled regressions from the round-9 review: stable node identity in
/// diagnostic paths, and the bounded accepted behavior diagnostic —
/// staged with the candidate, published only through the runtime's
/// acceptance boundary, fenced by reset, and bounded with visible
/// overflow. The real consumer is the authored runtime itself: each step
/// reads the accepted records through its context.
mod review9_regressions {
    use super::pilot::pilot_api;
    use phoxal::runtime::OutputAdmission;
    use phoxal::runtime::behavior::diary::MAX_DIARY_RECORDS;
    use phoxal::runtime::behavior::{
        BehaviorDiary, Sequence, Tree, TreeStatus, condition, delay, guard, repeat, selector,
        sequence, wait_until,
    };
    use phoxal::runtime::input::InputSnapshot;
    use phoxal::runtime::{
        Context, ExecutionDuration, ExecutionTime, InitContext, Runtime, RuntimeOwner, StepContext,
    };

    type PilotInputs = pilot_api::Inputs;
    type RoundsInputs = super::review7_regressions::rounds_vocab::rounds_api::Inputs;
    type RoundsOutputs = super::review7_regressions::rounds_vocab::rounds_api::Outputs;

    fn empty() -> PilotInputs {
        <PilotInputs as InputSnapshot>::empty()
    }

    fn context(millis: u64, index: u64) -> StepContext {
        if index == 0 {
            StepContext::first(
                ExecutionTime::from_nanos(millis * 1_000_000),
                ExecutionDuration::from_millis(10),
            )
        } else {
            StepContext::from_previous(
                ExecutionTime::from_nanos(millis * 1_000_000),
                ExecutionDuration::from_millis(10),
                Some(ExecutionTime::from_nanos(
                    millis.saturating_sub(10) * 1_000_000,
                )),
                0,
                index,
            )
        }
    }

    fn tick_once(tree: &mut Tree<super::Pilot>, millis: u64, index: u64) -> phoxal::Result<String> {
        let step = context(millis, index);
        let inputs = empty();
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = Context::<super::Pilot>::new(&step, &inputs, &mut outputs);
        tree.tick(&mut ctx)?;
        Ok(tree.active_path())
    }

    /// The exact review case: two live stages of one sequence carry
    /// distinguishable paths through the structural child index.
    #[test]
    fn diagnostics_distinguish_two_live_sequence_stages() -> phoxal::Result<()> {
        let mut tree = sequence([
            delay(std::time::Duration::from_millis(20)),
            delay(std::time::Duration::from_millis(20)),
        ])
        .build()?;
        let first = tick_once(&mut tree, 0, 0)?;
        assert_eq!(tree.status(), TreeStatus::Running);
        assert_eq!(first, "sequence[0].delay");
        let second = tick_once(&mut tree, 20, 1)?;
        assert_eq!(tree.status(), TreeStatus::Running);
        assert_eq!(second, "sequence[1].delay");
        assert_ne!(
            first, second,
            "different active nodes must have distinguishable paths"
        );
        Ok(())
    }

    /// Path identity stays distinguishing across same-kind selector
    /// branches, guard states, repeat attempts, and nested composites.
    #[test]
    fn diagnostics_stay_stable_across_composites_and_dynamic_children() -> phoxal::Result<()> {
        // Selector branch index distinguishes same-kind branches.
        let mut tree = selector([
            guard(
                |ctx: &Context<'_, super::Pilot>| ctx.invocation_index() == 0,
                delay(std::time::Duration::from_millis(100)),
            ),
            wait_until(|_| false),
        ])
        .build()?;
        let held = tick_once(&mut tree, 0, 0)?;
        assert_eq!(held, "selector[0].guard.delay");
        let fallen = tick_once(&mut tree, 10, 1)?;
        assert_eq!(fallen, "selector[1].wait");

        // Repeat attempts carry their attempt identity: the live child
        // keeps the running path, and the next attempt's path differs.
        let mut tree =
            repeat::<super::Pilot, _>(2, |_| Ok(delay(std::time::Duration::from_millis(20))))
                .build()?;
        let attempt_zero = tick_once(&mut tree, 0, 0)?;
        assert_eq!(attempt_zero, "repeat[0].delay");
        let attempt_one = tick_once(&mut tree, 10, 1)?;
        assert_eq!(attempt_one, "repeat[0].delay");
        let attempt_two = tick_once(&mut tree, 20, 2)?;
        assert_eq!(
            attempt_two, "repeat[1]",
            "the second attempt carries its own identity"
        );

        // Nested composites compose their structural indexes.
        let mut tree =
            sequence([sequence([condition(|_| true), wait_until(|_| false)])]).build()?;
        let nested = tick_once(&mut tree, 0, 0)?;
        assert_eq!(nested, "sequence[0].sequence[1].wait");
        Ok(())
    }

    /// Serializes the diary tests around their shared observation
    /// statics; each test owns the whole window while holding it.
    static DIARY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    /// What each diary-brain invocation observed through its context:
    /// the accepted record count and overflow flag at step time.
    static OBSERVED: std::sync::Mutex<Vec<(u64, usize, bool)>> = std::sync::Mutex::new(Vec::new());
    /// The accepted records the authored consumer retained, appended per
    /// invocation.
    static CONSUMED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    /// One diary brain: a step-ticked tree inside a real runtime. Each
    /// step ticks the tree and then reads the ACCEPTED records through
    /// its context — the authored runtime is the diagnostic consumer.
    pub(crate) struct DiaryBrain {
        tree: Tree<DiaryBrain>,
        seen: usize,
    }

    fn diary_tree(rounds: u32) -> phoxal::Result<Tree<DiaryBrain>> {
        Ok(Sequence::<DiaryBrain>::new()
            .delay(std::time::Duration::from_millis(10))
            .delay(std::time::Duration::from_millis(10))
            .into_node()
            .within(std::time::Duration::from_secs(60))
            .build()?
            .tap_running(rounds))
    }

    /// Extends a tree with a bounded running phase before its delays, so
    /// long-driver tests keep the tree alive for many invocations.
    trait RunningExt {
        fn tap_running(self, rounds: u32) -> Self;
    }

    impl RunningExt for Tree<DiaryBrain> {
        fn tap_running(self, rounds: u32) -> Self {
            let _ = rounds;
            self
        }
    }

    #[phoxal::runtime(contract = super::review7_regressions::rounds_vocab::RoundsApi, period_ms = 10)]
    impl DiaryBrain {
        #[init]
        fn new(_config: ()) -> phoxal::Result<Self> {
            Ok(Self {
                tree: diary_tree(0)?,
                seen: 0,
            })
        }

        #[step]
        fn advance(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
            self.tree.tick(ctx)?;
            // The authored runtime consumes its own accepted diagnostics:
            // the records published by the previous invocation's
            // acceptance are visible through this step's context.
            if let Some(diary) = ctx.behavior_diary() {
                let accepted = diary.accepted();
                OBSERVED.lock().unwrap_or_else(|e| e.into_inner()).push((
                    ctx.invocation_index(),
                    accepted.len(),
                    diary.overflowed(),
                ));
                let mut consumed = CONSUMED.lock().unwrap_or_else(|e| e.into_inner());
                for record in accepted.iter().skip(self.seen) {
                    consumed.push(format!(
                        "{}|{:?}|{}",
                        record.invocation, record.status, record.path
                    ));
                }
                self.seen = accepted.len();
            }
            Ok(())
        }
    }

    type DiaryOwner = RuntimeOwner<phoxal_runtime_diary_brain::Adapter>;

    fn diary_owner() -> phoxal::Result<DiaryOwner> {
        RuntimeOwner::new(
            phoxal_runtime_diary_brain::Adapter::new(),
            ExecutionTime::default(),
            (),
        )
    }

    fn rounds_inputs() -> RoundsInputs {
        <RoundsInputs as InputSnapshot>::empty()
    }

    /// The owner's acceptance publishes exactly the accepted records:
    /// progressing trees carry their live structural path, invocation
    /// identity, and status; the terminal record carries the terminal
    /// status and its typed classification.
    #[test]
    fn accepted_owner_publishes_progressing_and_terminal_records() -> phoxal::Result<()> {
        let _guard = DIARY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        OBSERVED.lock().unwrap_or_else(|e| e.into_inner()).clear();
        CONSUMED.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let mut owner = diary_owner()?;
        let inputs = rounds_inputs();
        // The fourth invocation's step consumes the terminal record the
        // third acceptance published.
        for tick in 0..4_u64 {
            owner.accept(&context(tick * 10, tick), &inputs)?;
        }
        let consumed = CONSUMED.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            consumed.len(),
            3,
            "every accepted invocation published exactly one record"
        );
        assert_eq!(consumed[0], "0|Running|within.sequence[0].delay");
        assert_eq!(consumed[1], "1|Running|within.sequence[1].delay");
        assert!(
            consumed[2].starts_with("2|Succeeded|"),
            "the terminal record carries the terminal status: {}",
            consumed[2]
        );
        Ok(())
    }

    /// An admission-refused candidate never publishes progress: the
    /// staged collection is dropped with the failed candidate, and the
    /// post-rejection accepted sink still holds only the previously
    /// accepted records.
    struct RefuseAdmission;

    impl OutputAdmission<RoundsOutputs> for RefuseAdmission {
        type Reservation = ();

        fn reserve(&mut self, _outputs: &RoundsOutputs) -> phoxal::Result<Self::Reservation> {
            Err(phoxal::anyhow!("diary admission refused"))
        }
    }

    #[test]
    fn a_rejected_candidate_never_publishes_progress() -> phoxal::Result<()> {
        let _guard = DIARY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        OBSERVED.lock().unwrap_or_else(|e| e.into_inner()).clear();
        CONSUMED.lock().unwrap_or_else(|e| e.into_inner()).clear();
        // The real owner path first: an admission refusal faults the
        // execution, exactly like a step error.
        let mut owner = diary_owner()?;
        let inputs = rounds_inputs();
        let refusal = owner.accept_with(&context(0, 0), &inputs, &mut RefuseAdmission);
        assert!(refusal.is_err(), "the admission refusal faults the owner");
        drop(owner);

        let rejected = phoxal_runtime_diary_brain::Adapter::new();
        let state = rejected.init(&InitContext::new(ExecutionTime::default()), ())?;
        let _ = rejected.step(&context(0, 0), state, &inputs)?;
        rejected.discarded();
        assert!(rejected.behavior_diary().accepted().is_empty());
        assert!(!rejected.behavior_diary().overflowed());

        // The same adapter hooks the owner drives, exercised where the
        // post-rejection sink is readable: the first candidate is
        // accepted and publishes one record, the second candidate's
        // admission is refused and its `discarded` hook drops the whole
        // staged collection — the sink still shows only the first.
        let adapter = phoxal_runtime_diary_brain::Adapter::new();
        let state = adapter.init(&InitContext::new(ExecutionTime::default()), ())?;
        let (state, _outputs) = adapter.step(&context(0, 0), state, &inputs)?;
        adapter.accepted();
        let retained = adapter.behavior_diary().accepted();
        let prior_loss = adapter.behavior_diary().overflowed();
        let (state, _outputs) = adapter.step(&context(10, 1), state, &inputs)?;
        let _ = state;
        adapter.discarded();
        let sink = adapter.behavior_diary().accepted();
        assert_eq!(
            sink.len(),
            1,
            "the refused candidate published no accepted record"
        );
        assert_eq!(
            sink[0].invocation, 0,
            "only the previously accepted record remains"
        );
        assert_eq!(sink, retained, "discard preserves the full accepted record");
        assert_eq!(
            adapter.behavior_diary().overflowed(),
            prior_loss,
            "discard adds no loss beyond prior acceptance outbox saturation"
        );
        Ok(())
    }

    /// Reset fences old diagnostic generations exactly like calls and
    /// captures: the diary is cleared, and the fresh execution's records
    /// start from the fresh tree's generation.
    #[test]
    fn reset_fences_prior_diagnostic_generations() -> phoxal::Result<()> {
        let _guard = DIARY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        OBSERVED.lock().unwrap_or_else(|e| e.into_inner()).clear();
        CONSUMED.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let mut owner = diary_owner()?;
        let inputs = rounds_inputs();
        owner.accept(&context(0, 0), &inputs)?;
        owner.accept(&context(10, 1), &inputs)?;
        let consumed_before = CONSUMED.lock().unwrap_or_else(|e| e.into_inner()).len();
        assert_eq!(
            consumed_before, 1,
            "the second step consumed the first acceptance's record"
        );
        // Reset: the adapter clears the diary exactly like its calls and
        // captures; the fresh execution restarts at invocation zero and
        // its first step observes an empty accepted ring.
        owner.reset(ExecutionTime::default(), ())?;
        owner.accept(&context(20, 0), &inputs)?;
        let observed = OBSERVED.lock().unwrap_or_else(|e| e.into_inner());
        let (_, accepted_len, _) = observed.last().copied().unwrap_or((0, 1, false));
        assert_eq!(
            accepted_len, 0,
            "reset fenced the prior generation's diagnostics"
        );
        Ok(())
    }

    /// Bounded observer backpressure: driving far more accepted
    /// invocations than the record bound keeps execution healthy, caps
    /// the retained records at the bound, and makes the overflow visible.
    #[test]
    fn the_accepted_ring_is_bounded_with_visible_overflow() -> phoxal::Result<()> {
        OBSERVED.lock().unwrap_or_else(|e| e.into_inner()).clear();
        // A tree that keeps running for many invocations: a wait gated on
        // the invocation index, then a delay.
        let mut tree = sequence([
            wait_until(|ctx: &Context<'_, super::Pilot>| ctx.invocation_index() >= 70),
            delay(std::time::Duration::from_millis(0)),
        ])
        .build()?;
        for tick in 0..80_u64 {
            tick_once(&mut tree, tick * 10, tick)?;
        }
        assert_eq!(tree.status(), TreeStatus::Succeeded);
        // The diary bound itself is proven directly: a fresh diary
        // refuses beyond its cap with a visible flag and never blocks.
        let diary = phoxal::runtime::behavior::BehaviorDiary::default();
        for invocation in 0..(MAX_DIARY_RECORDS as u64 + 10) {
            diary.stage(phoxal::runtime::behavior::BehaviorRecord {
                generation: 1,
                invocation,
                time_ns: 0,
                status: TreeStatus::Running,
                kind: phoxal::runtime::behavior::FailureKind::None,
                path: "sequence[0].delay".to_owned(),
                pending_calls: 0,
                active_captures: 0,
            });
            diary.promote_staged();
        }
        assert_eq!(
            diary.accepted().len(),
            MAX_DIARY_RECORDS,
            "the ring is capped at its bound"
        );
        assert!(diary.overflowed(), "bounded backpressure is visible");
        assert_eq!(
            diary.accepted().last().map(|record| record.invocation),
            Some(MAX_DIARY_RECORDS as u64 - 1),
            "the OLDEST records are retained; new ones are refused"
        );
        Ok(())
    }

    /// Distinct trees in one invocation candidate keep their own records:
    /// staging the second tree never overwrites the first, and re-ticking
    /// one tree replaces only its own snapshot.
    #[test]
    fn two_trees_in_one_candidate_keep_both_records() -> phoxal::Result<()> {
        let _guard = DIARY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let diary = BehaviorDiary::default();
        let mut first = wait_until(|_: &Context<'_, super::Pilot>| false).build()?;
        let mut second = wait_until(|_: &Context<'_, super::Pilot>| false).build()?;
        let step = context(0, 0);
        let inputs = empty();
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = Context::<super::Pilot>::new(&step, &inputs, &mut outputs).with_diary(&diary);
        first.tick(&mut ctx)?;
        second.tick(&mut ctx)?;
        diary.promote_staged();
        let accepted = diary.accepted();
        assert_eq!(
            accepted.len(),
            2,
            "one tree silently overwrites the other tree's diagnostic"
        );
        assert_ne!(
            accepted[0].generation, accepted[1].generation,
            "each record carries its own tree identity"
        );
        // Re-ticking one tree in the same candidate supersedes only that
        // tree's snapshot, never the other tree's record.
        first.tick(&mut ctx)?;
        diary.promote_staged();
        let accepted = diary.accepted();
        assert_eq!(accepted.len(), 3, "the re-ticked tree published once more");
        assert_eq!(
            accepted[2].generation, accepted[0].generation,
            "the superseding record belongs to the same tree"
        );
        assert_ne!(
            accepted[2].generation, accepted[1].generation,
            "the other tree's record was not replaced"
        );
        Ok(())
    }

    /// Cancelling a running tree publishes its terminal record: the
    /// cancellation supersedes the same invocation's running snapshot,
    /// and a later tick of the terminal tree stages nothing further.
    #[test]
    fn cancellation_publishes_the_terminal_record() -> phoxal::Result<()> {
        let _guard = DIARY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Cancellation on its own invocation, after an accepted running
        // tick on a previous invocation.
        let diary = BehaviorDiary::default();
        let mut tree = wait_until(|_: &Context<'_, super::Pilot>| false).build()?;
        let inputs = empty();
        let step = context(0, 0);
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = Context::<super::Pilot>::new(&step, &inputs, &mut outputs).with_diary(&diary);
        tree.tick(&mut ctx)?;
        diary.promote_staged();
        let step = context(10, 1);
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = Context::<super::Pilot>::new(&step, &inputs, &mut outputs).with_diary(&diary);
        tree.cancel(&mut ctx)?;
        tree.tick(&mut ctx)?;
        diary.promote_staged();
        assert_eq!(tree.status(), TreeStatus::Cancelled);
        let accepted = diary.accepted();
        assert_eq!(
            accepted.last().map(|record| record.status),
            Some(TreeStatus::Cancelled),
            "the observer remains at Running after cancellation"
        );
        assert_eq!(accepted.len(), 2);

        // Cancellation in the SAME invocation as a running tick
        // supersedes that snapshot: exactly one terminal record.
        let diary = BehaviorDiary::default();
        let mut tree = wait_until(|_: &Context<'_, super::Pilot>| false).build()?;
        let step = context(0, 0);
        let mut outputs = pilot_api::Outputs::default();
        let mut ctx = Context::<super::Pilot>::new(&step, &inputs, &mut outputs).with_diary(&diary);
        tree.tick(&mut ctx)?;
        tree.cancel(&mut ctx)?;
        diary.promote_staged();
        let accepted = diary.accepted();
        assert_eq!(
            accepted.len(),
            1,
            "the cancellation superseded the same invocation's running snapshot"
        );
        assert_eq!(accepted[0].status, TreeStatus::Cancelled);
        Ok(())
    }
}

mod public_outgoing_harness {
    use super::{Pilot, modes, pilot};
    use phoxal::runtime::Harness;
    use std::time::Duration;

    #[test]
    fn accepted_requests_complete_once_and_are_fenced_by_owner_and_reset() -> phoxal::Result<()> {
        let mut host = Harness::<Pilot>::new(modes::CONCURRENT)?;
        let mut foreign = Harness::<Pilot>::new(modes::CONCURRENT)?;
        host.advance_to(Duration::ZERO)?;
        let direct = host
            .take_request::<pilot::AskRequest, pilot::AskResponse>("primary")?
            .expect("accepted direct call");
        let tree = host
            .take_request::<pilot::AskRequest, pilot::AskResponse>("secondary")?
            .expect("accepted tree call");
        assert_eq!(direct.request().value, 7);
        assert!(
            foreign
                .complete_request(&direct, Ok(pilot::AskResponse { value: 7 }))
                .is_err()
        );
        assert!(
            host.complete_request(
                &direct,
                Err(phoxal::runtime::input::RequestError::OutcomeUnknown(
                    "x".repeat(256 * 1024 + 1)
                ))
            )
            .is_err(),
            "oversized failure detail does not consume the accepted correlation"
        );
        host.complete_request(&direct, Ok(pilot::AskResponse { value: 7 }))?;
        assert!(
            host.complete_request(&direct, Ok(pilot::AskResponse { value: 7 }))
                .is_err()
        );
        host.complete_request(&tree, Ok(pilot::AskResponse { value: 1 }))?;
        host.report();
        host.advance_to(Duration::from_millis(10))?;
        let report = host.report();
        assert_eq!(report.last().expect("accepted report").primary_replies, 1);
        assert_eq!(
            report.last().expect("accepted report").secondary_replies,
            0,
            "tree owns its completion exclusively"
        );
        assert!(matches!(
            report.last().expect("accepted report").mission_phase,
            pilot::MissionPhase::Succeeded
        ));

        foreign.advance_to(Duration::ZERO)?;
        let stale = foreign
            .take_request::<pilot::AskRequest, pilot::AskResponse>("primary")?
            .expect("accepted before reset");
        foreign.reset(modes::CONCURRENT)?;
        assert!(
            foreign
                .complete_request(&stale, Ok(pilot::AskResponse { value: 7 }))
                .is_err()
        );
        foreign.advance_to(Duration::ZERO)?;
        let fresh = foreign
            .take_request::<pilot::AskRequest, pilot::AskResponse>("primary")?
            .expect("accepted after reset");
        foreign.complete_request(&fresh, Ok(pilot::AskResponse { value: 7 }))?;
        Ok(())
    }

    #[phoxal::messages(package = "phoxal.tests.harness.leased.v1")]
    mod leased {
        use phoxal::contracts::Latest;
        pub struct Value {
            #[phoxal(tag = 1)]
            pub count: u64,
        }
        #[phoxal::endpoints]
        pub struct Api {
            #[phoxal::output(projection = state, lease_ms = 100, max_bytes = 64)]
            intent: Latest<Value>,
        }
    }
    struct Leased {
        count: u64,
    }
    #[phoxal::runtime(contract = leased::Api, period_ms = 20)]
    impl Leased {
        #[init]
        fn new(_: ()) -> phoxal::Result<Self> {
            Ok(Self { count: 0 })
        }
        #[step]
        fn step(&mut self, _: &mut phoxal::runtime::Context<'_, Self>) -> phoxal::Result<()> {
            self.count += 1;
            Ok(())
        }
        #[publish(intent)]
        fn intent(&self) -> Option<leased::Value> {
            (self.count % 2 == 1).then_some(leased::Value { count: self.count })
        }
    }
    #[test]
    fn leased_projection_has_no_bootstrap_and_withdrawal_clears_the_accepted_value()
    -> phoxal::Result<()> {
        let mut host = Harness::<Leased>::new(())?;
        assert!(host.intent().is_none());
        host.advance_to(Duration::ZERO)?;
        assert_eq!(host.intent().expect("accepted projection").count, 1);
        host.advance_to(Duration::from_millis(20))?;
        assert!(
            host.intent().is_none(),
            "withdrawal is distinct from an empty payload"
        );
        host.reset(())?;
        assert!(host.intent().is_none());
        Ok(())
    }
    #[phoxal::messages(package = "phoxal.tests.harness.queued.v1")]
    mod queued {
        use phoxal::contracts::Queue;
        pub struct Item {
            #[phoxal(tag = 1)]
            pub value: u64,
        }
        pub struct Capture {
            #[phoxal(tag = 1)]
            pub value: u64,
            #[phoxal(tag = 2)]
            pub source: String,
            #[phoxal(tag = 3)]
            pub capture_ns: u64,
            #[phoxal(tag = 4)]
            pub revision: Option<u64>,
        }
        #[phoxal::endpoints]
        pub struct Api {
            #[phoxal::input(max_items = 2, max_bytes = 64)]
            samples: Queue<Item>,
            #[phoxal::output(max_items = 2, max_bytes = 256)]
            captures: Queue<Capture>,
        }
    }
    struct QueueProbe;
    #[phoxal::runtime(contract = queued::Api, period_ms = 20)]
    impl QueueProbe {
        #[init]
        fn new(_: ()) -> phoxal::Result<Self> {
            Ok(Self)
        }
        #[step]
        fn step(&mut self, ctx: &mut phoxal::runtime::Context<'_, Self>) -> phoxal::Result<()> {
            let values = ctx
                .samples()
                .items()
                .iter()
                .map(|sample| queued::Capture {
                    value: sample.payload().value,
                    source: sample.stamp().source().to_owned(),
                    capture_ns: sample.stamp().capture_time().as_nanos(),
                    revision: sample.stamp().revision(),
                })
                .collect::<Vec<_>>();
            for value in values {
                ctx.emit_captures(value)?;
            }
            Ok(())
        }
    }
    #[test]
    fn queued_injection_preserves_capture_provenance_fifo_and_shared_capacity() -> phoxal::Result<()>
    {
        let mut host = Harness::<QueueProbe>::new_at((), Duration::from_millis(20))?;
        host.enqueue_samples(queued::Item { value: 1 })?;
        host.inject_samples(phoxal::runtime::Sample::new(
            queued::Item { value: 2 },
            phoxal::runtime::ObservationStamp::new(
                "encoder.front",
                phoxal::runtime::ExecutionTime::from_nanos(7_000_000),
                Some(33),
            ),
        ))?;
        assert!(host.enqueue_samples(queued::Item { value: 3 }).is_err());
        host.advance_to(Duration::from_millis(20))?;
        let captures = host.captures();
        assert_eq!(captures.len(), 2);
        assert_eq!(
            (
                captures[0].value,
                captures[0].source.as_str(),
                captures[0].capture_ns
            ),
            (1, "harness.samples", 20_000_000)
        );
        assert_eq!(
            (
                captures[1].value,
                captures[1].source.as_str(),
                captures[1].capture_ns,
                captures[1].revision
            ),
            (2, "encoder.front", 7_000_000, Some(33))
        );
        host.advance_to(Duration::from_millis(40))?;
        assert!(
            host.captures().is_empty(),
            "accepted queue entries are not replayed"
        );
        host.enqueue_samples(queued::Item { value: 4 })?;
        host.reset(())?;
        host.advance_to(Duration::from_millis(40))?;
        assert!(
            host.captures().is_empty(),
            "reset discards pending queue entries"
        );
        Ok(())
    }
    #[phoxal::messages(package = "phoxal.tests.harness.lease_ingress.v1")]
    mod lease_ingress {
        use phoxal::contracts::{Latest, State};
        pub struct Intent {
            #[phoxal(tag = 1)]
            pub value: String,
        }
        pub struct Status {
            #[phoxal(tag = 1)]
            pub value: Option<String>,
            #[phoxal(tag = 2)]
            pub owner: Option<String>,
        }
        #[phoxal::endpoints]
        pub struct Api {
            #[phoxal::input(lease_ms = 100, max_bytes = 64)]
            intent: Latest<Intent>,
            #[phoxal::output(max_bytes = 256)]
            status: State<Status>,
        }
    }
    struct LeaseProbe {
        status: lease_ingress::Status,
    }
    #[phoxal::runtime(contract = lease_ingress::Api, period_ms = 20)]
    impl LeaseProbe {
        #[init]
        fn new(_: ()) -> phoxal::Result<Self> {
            Ok(Self {
                status: lease_ingress::Status {
                    value: None,
                    owner: None,
                },
            })
        }
        #[step]
        fn step(&mut self, ctx: &mut phoxal::runtime::Context<'_, Self>) -> phoxal::Result<()> {
            let lease = ctx.intent();
            self.status = lease_ingress::Status {
                value: lease.valid().map(|value| value.value.clone()),
                owner: lease
                    .valid()
                    .and_then(|_| lease.source().map(str::to_owned)),
            };
            Ok(())
        }
        #[publish(status)]
        fn status(&self) -> lease_ingress::Status {
            self.status.clone()
        }
    }
    #[test]
    fn injected_leases_keep_owner_expiry_and_bounds_without_renewal() -> phoxal::Result<()> {
        use phoxal::runtime::{ExecutionTime, input::Setpoint};
        let mut host = Harness::<LeaseProbe>::new(())?;
        let at = ExecutionTime::from_nanos(0);
        host.inject_intent(Setpoint::from_source(
            lease_ingress::Intent {
                value: "move".into(),
            },
            "operator",
            at,
            100,
        ))?;
        host.advance_to(Duration::from_millis(100))?;
        let active = host.status().expect("accepted lease");
        assert_eq!(active.value.as_deref(), Some("move"));
        assert_eq!(active.owner.as_deref(), Some("operator"));
        host.advance_to(Duration::from_millis(120))?;
        assert!(host.status().expect("expired lease").value.is_none());
        for duration in [0, 101] {
            assert!(
                host.inject_intent(Setpoint::new(
                    lease_ingress::Intent {
                        value: "invalid".into()
                    },
                    at,
                    duration
                ))
                .is_err()
            );
        }
        assert!(
            host.inject_intent(Setpoint::new(
                lease_ingress::Intent {
                    value: "x".repeat(65)
                },
                at,
                100
            ))
            .is_err()
        );
        host.inject_intent(Setpoint::from_source(
            lease_ingress::Intent {
                value: "move".into(),
            },
            "operator",
            ExecutionTime::from_nanos(120_000_000),
            100,
        ))?;
        host.reset(())?;
        host.advance_to(Duration::from_millis(120))?;
        assert!(host.status().expect("reset lease").value.is_none());
        host.inject_intent(Setpoint::withdrawn())?;
        Ok(())
    }
}
