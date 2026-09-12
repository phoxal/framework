//! The synchronous Runtime authoring contract and its execution facts.
//!
//! A [`Runtime`] owns a private state value and turns one immutable input cut
//! into a fresh output value.  This module deliberately contains no transport
//! or scheduler implementation.  Those owners can use the same contract for
//! hardware, controlled simulation, and the public in-process adapter.

use std::fmt;
use std::time::Duration;

/// Configuration accepted by a [`Runtime`].
///
/// Configuration is deserialized once at the owning boundary and then passed
/// by value to [`Runtime::init`].  The blanket implementation keeps the
/// existing `#[derive(phoxal::Config)]` schema machinery available while the
/// runtime authoring surface is introduced.
pub trait Config: serde::de::DeserializeOwned + Send + 'static {
    /// The JSON Schema for the admitted configuration value.
    const SCHEMA_JSON: &'static str;
}

impl<T> Config for T
where
    T: crate::participant::config::ParticipantConfig,
{
    const SCHEMA_JSON: &'static str = T::SCHEMA_JSON;
}

/// A monotonic instant in the execution's logical time domain.
///
/// The value is represented in nanoseconds so it can be copied into a
/// [`StepContext`] without carrying a host clock or a transport handle.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExecutionTime(u64);

impl ExecutionTime {
    /// Creates an instant from nanoseconds since the execution origin.
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Returns nanoseconds since the execution origin.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Computes elapsed execution time without allowing the clock to run
    /// backwards.
    #[must_use]
    pub const fn checked_duration_since(self, earlier: Self) -> Option<ExecutionDuration> {
        match self.0.checked_sub(earlier.0) {
            Some(nanos) => Some(ExecutionDuration::from_nanos(nanos)),
            None => None,
        }
    }
}

impl From<Duration> for ExecutionTime {
    fn from(duration: Duration) -> Self {
        let nanos = duration.as_nanos().min(u64::MAX as u128) as u64;
        Self::from_nanos(nanos)
    }
}

impl From<ExecutionTime> for Duration {
    fn from(time: ExecutionTime) -> Self {
        Duration::from_nanos(time.0)
    }
}

/// Provenance attached to a captured or forwarded observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservationStamp {
    capture_time: ExecutionTime,
    source: String,
    revision: Option<u64>,
}

impl ObservationStamp {
    /// Creates an observation stamp from its source, capture instant, and
    /// optional source revision.
    #[must_use]
    pub fn new(
        source: impl Into<String>,
        capture_time: ExecutionTime,
        revision: Option<u64>,
    ) -> Self {
        Self {
            capture_time,
            source: source.into(),
            revision,
        }
    }

    /// Returns the source's capture instant.
    #[must_use]
    pub const fn capture_time(&self) -> ExecutionTime {
        self.capture_time
    }

    /// Returns the source identity.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Returns the source revision when one exists.
    #[must_use]
    pub const fn revision(&self) -> Option<u64> {
        self.revision
    }
}

/// A payload together with the source observation stamp.
pub struct Sample<T> {
    payload: T,
    stamp: ObservationStamp,
}

impl<T> Sample<T> {
    /// Creates a stamped local observation.
    #[must_use]
    pub fn new(payload: T, stamp: ObservationStamp) -> Self {
        Self { payload, stamp }
    }

    /// Returns the observed payload.
    #[must_use]
    pub fn payload(&self) -> &T {
        &self.payload
    }

    /// Returns its original source and capture metadata.
    #[must_use]
    pub const fn stamp(&self) -> &ObservationStamp {
        &self.stamp
    }

    /// Consumes the sample into payload and provenance.
    #[must_use]
    pub fn into_parts(self) -> (T, ObservationStamp) {
        (self.payload, self.stamp)
    }
}

impl<T: fmt::Debug> fmt::Debug for Sample<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Sample")
            .field("payload", &self.payload)
            .field("stamp", &self.stamp)
            .finish()
    }
}

/// A non-negative duration in the execution's logical time domain.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExecutionDuration(u64);

impl ExecutionDuration {
    /// Creates a duration from nanoseconds.
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Creates a duration from milliseconds, saturating on overflow.
    #[must_use]
    pub const fn from_millis(milliseconds: u64) -> Self {
        Self(milliseconds.saturating_mul(1_000_000))
    }

    /// Returns the duration in nanoseconds.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Returns the duration in milliseconds, rounded down.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0 / 1_000_000
    }

    /// Returns the standard-library representation.
    #[must_use]
    pub const fn as_duration(self) -> Duration {
        Duration::from_nanos(self.0)
    }
}

impl From<Duration> for ExecutionDuration {
    fn from(duration: Duration) -> Self {
        let nanos = duration.as_nanos().min(u64::MAX as u128) as u64;
        Self::from_nanos(nanos)
    }
}

impl From<ExecutionDuration> for Duration {
    fn from(duration: ExecutionDuration) -> Self {
        duration.as_duration()
    }
}

/// Facts available while a runtime initializes its private state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InitContext {
    now: ExecutionTime,
}

impl InitContext {
    /// Creates an initialization context at the supplied logical instant.
    #[must_use]
    pub const fn new(now: ExecutionTime) -> Self {
        Self { now }
    }

    /// Returns the logical instant at which initialization was admitted.
    #[must_use]
    pub const fn now(self) -> ExecutionTime {
        self.now
    }
}

impl Default for InitContext {
    fn default() -> Self {
        Self::new(ExecutionTime::from_nanos(0))
    }
}

/// Read-only facts for one accepted runtime invocation candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StepContext {
    now: ExecutionTime,
    period: ExecutionDuration,
    elapsed: ExecutionDuration,
    missed_releases: u64,
    invocation_index: u64,
}

impl StepContext {
    /// Creates a context with explicit scheduler facts.
    #[must_use]
    pub const fn new(
        now: ExecutionTime,
        period: ExecutionDuration,
        elapsed: ExecutionDuration,
        missed_releases: u64,
        invocation_index: u64,
    ) -> Self {
        Self {
            now,
            period,
            elapsed,
            missed_releases,
            invocation_index,
        }
    }

    /// Creates the first context for a runtime.
    #[must_use]
    pub const fn first(now: ExecutionTime, period: ExecutionDuration) -> Self {
        Self::new(now, period, ExecutionDuration::from_nanos(0), 0, 0)
    }

    /// Creates a context from two accepted logical timestamps.
    #[must_use]
    pub const fn from_previous(
        now: ExecutionTime,
        period: ExecutionDuration,
        previous: Option<ExecutionTime>,
        missed_releases: u64,
        invocation_index: u64,
    ) -> Self {
        let elapsed = match previous {
            Some(previous) => match now.checked_duration_since(previous) {
                Some(elapsed) => elapsed,
                None => ExecutionDuration::from_nanos(0),
            },
            None => ExecutionDuration::from_nanos(0),
        };
        Self::new(now, period, elapsed, missed_releases, invocation_index)
    }

    /// Returns the logical instant of the frozen input cut.
    #[must_use]
    pub const fn now(self) -> ExecutionTime {
        self.now
    }

    /// Returns the source-authored nominal period.
    #[must_use]
    pub const fn period(self) -> ExecutionDuration {
        self.period
    }

    /// Returns the elapsed logical time since the previous accepted
    /// invocation, or zero for the first invocation.
    #[must_use]
    pub const fn elapsed(self) -> ExecutionDuration {
        self.elapsed
    }

    /// Returns nominal releases skipped before this selected release.
    #[must_use]
    pub const fn missed_releases(self) -> u64 {
        self.missed_releases
    }

    /// Returns the zero-based accepted invocation index.
    #[must_use]
    pub const fn invocation_index(self) -> u64 {
        self.invocation_index
    }
}

impl Default for StepContext {
    fn default() -> Self {
        Self::first(
            ExecutionTime::from_nanos(0),
            ExecutionDuration::from_nanos(0),
        )
    }
}

/// Static runtime cadence and deadline metadata emitted by the root runtime
/// attribute.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeSpec {
    /// Nominal logical period.
    pub period: ExecutionDuration,
    /// Host-monotonic complete-invocation deadline.
    pub timeout: ExecutionDuration,
    /// Host-monotonic initialization deadline.
    pub init_timeout: ExecutionDuration,
}

impl RuntimeSpec {
    /// Creates runtime metadata from authored millisecond values.
    #[must_use]
    pub const fn from_millis(period_ms: u64, timeout_ms: u64, init_timeout_ms: u64) -> Self {
        Self {
            period: ExecutionDuration::from_millis(period_ms),
            timeout: ExecutionDuration::from_millis(timeout_ms),
            init_timeout: ExecutionDuration::from_millis(init_timeout_ms),
        }
    }

    /// Validates the hard lower bound shared by every runtime registration.
    pub const fn validate(self) -> Result<(), RuntimeSpecError> {
        if self.period.as_nanos() == 0 {
            return Err(RuntimeSpecError::ZeroPeriod);
        }
        if self.timeout.as_nanos() == 0 {
            return Err(RuntimeSpecError::ZeroTimeout);
        }
        if self.init_timeout.as_nanos() == 0 {
            return Err(RuntimeSpecError::ZeroInitTimeout);
        }
        Ok(())
    }
}

/// An invalid root runtime timing declaration.
#[allow(
    clippy::enum_variant_names,
    reason = "the error variants identify the three distinct authored timing fields"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeSpecError {
    /// The logical period must be positive.
    ZeroPeriod,
    /// The invocation deadline must be positive.
    ZeroTimeout,
    /// The initialization deadline must be positive.
    ZeroInitTimeout,
}

impl fmt::Display for RuntimeSpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ZeroPeriod => "runtime period must be positive",
            Self::ZeroTimeout => "runtime timeout must be positive",
            Self::ZeroInitTimeout => "runtime init timeout must be positive",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for RuntimeSpecError {}

/// A marker implemented by `#[phoxal::runtime(...)]`.
pub trait RegisteredRuntime: Runtime {
    /// The source-authored cadence and deadline metadata.
    const SPEC: RuntimeSpec;
}

/// The direct, synchronous service behavior contract.
pub trait Runtime {
    /// The exact admitted configuration type.
    type Config: Config;
    /// Persistent state owned by one compute owner.
    type State;
    /// Immutable input snapshot for one invocation.
    type Inputs;
    /// Fresh transient output batches for one invocation.
    type Outputs;

    /// Validates business invariants after typed configuration decoding and
    /// before initialization reaches Ready.
    fn validate_config(_config: &Self::Config) -> crate::Result<()> {
        Ok(())
    }

    /// Constructs persistent state from the admitted configuration.
    fn init(&self, ctx: &InitContext, config: Self::Config) -> crate::Result<Self::State>;

    /// Applies one immutable input cut and returns the next state plus fresh
    /// transient outputs.
    fn step(
        &self,
        ctx: &StepContext,
        state: Self::State,
        inputs: &Self::Inputs,
    ) -> crate::Result<(Self::State, Self::Outputs)>;
}

/// A stable identifier for one accepted invocation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Invocation {
    index: u64,
}

impl Invocation {
    /// Creates an invocation identity from its accepted index.
    #[must_use]
    pub const fn new(index: u64) -> Self {
        Self { index }
    }

    /// Returns the accepted invocation index.
    #[must_use]
    pub const fn index(self) -> u64 {
        self.index
    }
}

/// The lifecycle of a serialized runtime owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeStatus {
    /// Initialization succeeded and the owner accepts invocations.
    Ready,
    /// Initialization or an invocation failed; this execution is terminal.
    Failed,
}

/// The accepted products of one direct invocation.
pub struct AcceptedInvocation<Outputs> {
    invocation: Invocation,
    context: StepContext,
    outputs: Outputs,
}

impl<Outputs> AcceptedInvocation<Outputs> {
    /// Creates an accepted invocation record.
    #[must_use]
    pub const fn new(invocation: Invocation, context: StepContext, outputs: Outputs) -> Self {
        Self {
            invocation,
            context,
            outputs,
        }
    }

    /// Returns the stable accepted invocation identity.
    #[must_use]
    pub const fn invocation(&self) -> Invocation {
        self.invocation
    }

    /// Returns the immutable facts used for this accepted invocation.
    #[must_use]
    pub const fn context(&self) -> StepContext {
        self.context
    }

    /// Returns the transient output products.
    #[must_use]
    pub const fn outputs(&self) -> &Outputs {
        &self.outputs
    }

    /// Consumes the accepted products.
    #[must_use]
    pub fn into_parts(self) -> (Invocation, StepContext, Outputs) {
        (self.invocation, self.context, self.outputs)
    }

    /// Consumes the record and returns only its output products.
    #[must_use]
    pub fn into_outputs(self) -> Outputs {
        self.outputs
    }
}

/// Errors that an in-process adapter can report before accepting an
/// invocation.  Transport adapters may wrap these in their own failure
/// contracts.
#[derive(Debug, thiserror::Error)]
pub enum InvocationError {
    /// The runtime's static timing declaration is invalid.
    #[error("invalid runtime specification: {0}")]
    InvalidSpec(#[from] RuntimeSpecError),
    /// The candidate's invocation index does not follow the accepted prefix.
    #[error("invocation index {actual} follows {expected} accepted invocations")]
    UnexpectedIndex {
        /// Supplied index.
        actual: u64,
        /// Required next index.
        expected: u64,
    },
    /// The execution was already faulted by an earlier lifecycle failure.
    #[error("runtime execution is already failed")]
    Failed,
    /// The service panicked while preparing an invocation.
    #[error("runtime invocation panicked")]
    Panicked,
    /// The supplied logical clock moved backwards.
    #[error("invocation time moved backwards")]
    ClockReversed,
}

/// Validates a registered runtime and runs its initialization hook with an
/// owned configuration value.
#[allow(
    dead_code,
    reason = "the transport and public test adapters call this entrypoint"
)]
pub fn initialize<R: RegisteredRuntime>(
    service: &R,
    now: ExecutionTime,
    config: R::Config,
) -> crate::Result<R::State> {
    R::SPEC
        .validate()
        .map_err(|error| anyhow::anyhow!(InvocationError::from(error)))?;
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        R::validate_config(&config)?;
        service.init(&InitContext::new(now), config)
    })) {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(InvocationError::Panicked)),
    }
}

/// Runs one direct state transition through the same public adapter path used
/// by a host runner.
#[allow(
    dead_code,
    reason = "the transport and public test adapters call this entrypoint"
)]
pub fn invoke<R: Runtime>(
    service: &R,
    context: &StepContext,
    state: R::State,
    inputs: &R::Inputs,
) -> crate::Result<(R::State, R::Outputs)> {
    service.step(context, state, inputs)
}

/// A serialized, direct runtime owner for tests and local adapters.
///
/// The owner keeps [`Runtime::State`] private and commits a candidate state
/// only after `step` returns successfully.  A failed or panicking invocation
/// transitions the current execution to [`RuntimeStatus::Failed`]; callers
/// must create a fresh owner to resume with fresh initialization.
pub struct RuntimeOwner<R: RegisteredRuntime> {
    service: R,
    state: Option<R::State>,
    next_index: u64,
    last_time: Option<ExecutionTime>,
    status: RuntimeStatus,
}

impl<R: RegisteredRuntime> RuntimeOwner<R> {
    /// Initializes a serialized owner with the same validation order as the
    /// public initialization adapter.
    pub fn new(service: R, now: ExecutionTime, config: R::Config) -> crate::Result<Self> {
        let state = initialize(&service, now, config)?;
        Ok(Self {
            service,
            state: Some(state),
            next_index: 0,
            last_time: None,
            status: RuntimeStatus::Ready,
        })
    }

    /// Returns the lifecycle status without exposing private state.
    #[must_use]
    pub const fn status(&self) -> RuntimeStatus {
        self.status
    }

    /// Returns the next invocation index required for acceptance.
    #[must_use]
    pub const fn next_invocation(&self) -> Invocation {
        Invocation::new(self.next_index)
    }

    /// Returns a shared reference to the service implementation.
    #[must_use]
    pub const fn service(&self) -> &R {
        &self.service
    }

    /// Runs and accepts one serialized invocation.
    ///
    /// The candidate state and outputs are prepared together.  State and the
    /// accepted index advance only when the complete `step` result is `Ok`.
    /// Capacity reservation and transport publication belong to the owning
    /// host after this local acceptance boundary.
    pub fn accept(
        &mut self,
        context: &StepContext,
        inputs: &R::Inputs,
    ) -> crate::Result<AcceptedInvocation<R::Outputs>> {
        if self.status == RuntimeStatus::Failed {
            return Err(anyhow::anyhow!(InvocationError::Failed));
        }
        let expected = self.next_index;
        let actual = context.invocation_index();
        if actual != expected {
            return Err(anyhow::anyhow!(InvocationError::UnexpectedIndex {
                actual,
                expected,
            }));
        }
        if self
            .last_time
            .is_some_and(|previous| context.now() < previous)
        {
            return Err(anyhow::anyhow!(InvocationError::ClockReversed));
        }
        let state = self.state.take().ok_or_else(|| {
            self.status = RuntimeStatus::Failed;
            anyhow::anyhow!(InvocationError::Failed)
        })?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.service.step(context, state, inputs)
        }));
        let (next_state, outputs) = match result {
            Ok(Ok(products)) => products,
            Ok(Err(error)) => {
                self.status = RuntimeStatus::Failed;
                return Err(error);
            }
            Err(_) => {
                self.status = RuntimeStatus::Failed;
                return Err(anyhow::anyhow!(InvocationError::Panicked));
            }
        };
        self.state = Some(next_state);
        self.last_time = Some(context.now());
        self.next_index = self.next_index.saturating_add(1);
        Ok(AcceptedInvocation::new(
            Invocation::new(actual),
            *context,
            outputs,
        ))
    }
}

/// Process entrypoint placeholder for the transport-owned runner.
///
/// A runtime binary can expose `main` with this function while the selected
/// host supplies configuration, transport, and scheduling through its bundle
/// runner.  Returning a typed error is preferable to silently constructing a
/// default configuration or running an unbounded loop.
#[allow(dead_code, reason = "the transport-owned binary calls this entrypoint")]
pub fn run<R: RegisteredRuntime>(_service: R) -> crate::Result<()> {
    Err(anyhow::anyhow!(
        "the transport-owned runtime runner is not available in the direct authoring profile"
    ))
}

#[cfg(test)]
mod tests {
    use super::{ExecutionDuration, ExecutionTime, InitContext, StepContext};

    #[test]
    fn first_step_has_zero_elapsed_and_index() {
        let context = StepContext::first(
            ExecutionTime::from_nanos(7_000_000),
            ExecutionDuration::from_millis(20),
        );
        assert_eq!(context.now().as_nanos(), 7_000_000);
        assert_eq!(context.period().as_millis(), 20);
        assert_eq!(context.elapsed().as_nanos(), 0);
        assert_eq!(context.missed_releases(), 0);
        assert_eq!(context.invocation_index(), 0);
    }

    #[test]
    fn late_hardware_context_uses_actual_elapsed_time() {
        let context = StepContext::from_previous(
            ExecutionTime::from_nanos(68_000_000),
            ExecutionDuration::from_millis(20),
            Some(ExecutionTime::from_nanos(7_000_000)),
            2,
            1,
        );
        assert_eq!(context.elapsed().as_millis(), 61);
        assert_eq!(context.missed_releases(), 2);
        assert_eq!(context.invocation_index(), 1);
    }

    #[test]
    fn initialization_context_is_read_only_and_typed() {
        let context = InitContext::new(ExecutionTime::from_nanos(11));
        assert_eq!(context.now().as_nanos(), 11);
    }
}
