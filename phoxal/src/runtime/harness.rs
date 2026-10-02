//! Public explicit-time harness over the real runtime owner.
//!
//! [`Harness`] drives one authored runtime through the generated adapter
//! and [`RuntimeOwner`] with an explicit virtual clock: every due release
//! of the declared cadence executes exactly once, inputs are admitted
//! through the contract's real bounds, and outputs pass through complete
//! encoded-batch admission before their typed effects are retained for
//! inspection. There is no second dispatcher, state machine, or scheduler:
//! the harness owns only the clock, the release cursor, and bounded
//! test-side storage.
//!
//! Endpoint-specific methods (`enqueue_start`, `finished`, `status`) are
//! generated per contract and live on the owned endpoint view, reached
//! through `Deref`/`DerefMut` on the harness itself.
//!
//! The harness covers local state-transition and admission evidence only;
//! it is not transport, supervisor, or simulation-timeline evidence.

use std::ops::{Deref, DerefMut};
use std::time::Duration;

use super::EndpointView;
use super::RegisteredRuntime;
use super::core::{
    ExecutionDuration, ExecutionTime, OutputAdmission, Runtime, RuntimeOwner, RuntimeStatus,
    StepContext,
};
use super::input::{CallResponse, CommandId, InputSet};
use super::outputs::OutputSet;
use crate::port::PortSignature;

/// Upper bound of releases one `advance_to` operation may execute before
/// reporting work exhaustion. Accepted progress is preserved; the caller
/// resumes by advancing again.
pub const MAX_RELEASES_PER_ADVANCE: u64 = 10_000;

/// A runtime configuration document, as the launch path and the harness
/// exchange it.
pub type ConfigDocument = serde_json::Value;

/// Associates one authored runtime type with the harness endpoint view.
///
/// The runtime attachment implements this for the authored type; it is not
/// a hand-authoring surface. Every surface type is public or generated and
/// public-in-private: the private adapter and the authored configuration
/// type are named only inside generated implementation bodies, so a
/// private authored runtime never leaks through this public interface.
pub trait HarnessAttachment: EndpointView + Sized
where
    Self::Inputs: InputSet,
    Self::Outputs: OutputSet,
{
    /// The contract-owned harness endpoint view: staged pending inputs
    /// plus retained accepted effects, with the generated endpoint methods.
    type HarnessView: Default;

    /// Builds the owner driver from the configuration's JSON document.
    fn harness_driver(
        config: serde_json::Value,
    ) -> crate::Result<HarnessDriver<Self::Inputs, Self::Outputs>>;

    /// Freezes the staged inputs into one invocation's input cut, stamping
    /// queued items at the release instant.
    fn build_inputs(view: &mut Self::HarnessView, now: ExecutionTime) -> Self::Inputs;

    /// Retains one accepted invocation's typed transient outputs (replies
    /// and queued events) within the contract's bounds. State publications
    /// cross as encoded records through [`store_state_record`], so an
    /// authored runtime type never appears in this public interface.
    ///
    /// [`store_state_record`]: Self::store_state_record
    fn capture_outputs(view: &mut Self::HarnessView, outputs: &Self::Outputs) -> crate::Result<()>;

    /// Stores one encoded state publication, keyed by its output field
    /// name; the generated view decodes it lazily in its accessor.
    fn store_state_record(view: &mut Self::HarnessView, field: &'static str, bytes: Vec<u8>);

    /// Takes the encoded reply for one call correlation, if it has been
    /// accepted.
    fn take_reply_bytes(view: &mut Self::HarnessView, id: CommandId) -> Option<Vec<u8>>;

    /// The exclusive upper bound of issued call correlations; a call id at
    /// or above it was never issued, while an absent reply below it was
    /// consumed or belongs to a rejected candidate.
    fn call_upper_bound(view: &Self::HarnessView) -> u64;

    /// Reports whether one call correlation is still staged for a future
    /// release, so its reply is pending rather than consumed.
    fn call_is_staged(view: &Self::HarnessView, id: CommandId) -> bool;

    /// Validates that one candidate's transient outputs fit every
    /// endpoint's retained item and byte bounds together with the effects
    /// already retained, without mutating anything: the complete accepted
    /// effect is reserved before the owner accepts the candidate.
    fn validate_retention(view: &Self::HarnessView, outputs: &Self::Outputs) -> crate::Result<()>;

    /// Binds the view to one harness owner identity, stamped into every
    /// issued call correlation so a token from another harness can never
    /// consume this harness's replies.
    fn bind_harness(view: &mut Self::HarnessView, owner: u64);

    /// Clears every staged input and retained effect while keeping the
    /// call-correlation counter and harness identity, so a call enqueued
    /// before a reset can never alias a call enqueued after it.
    fn clear(view: &mut Self::HarnessView);
}

/// Object-safe operations over the real runtime owner, hiding the adapter
/// behind the input and output transaction types the harness already
/// knows.
pub(crate) trait OwnerOps<Inputs, Outputs> {
    /// Runs one invocation through the complete encoded-batch admission
    /// path and returns its outputs on acceptance.
    ///
    /// The complete candidate is validated before the owner accepts it:
    /// the transient batch is encoded and its retention capacity reserved
    /// through `validate`, and the hook re-encodes the candidate's state
    /// publications through the same bounded bindings, so any failure
    /// retires the owner through the real failure path with nothing
    /// accepted, no invocation index consumed, and no effect exposed.
    #[allow(clippy::type_complexity)]
    fn accept(
        &mut self,
        context: StepContext,
        inputs: &Inputs,
        ports: &[(&'static str, Option<PortSignature>)],
        validate: &(dyn Fn(&Outputs) -> crate::Result<()> + '_),
    ) -> crate::Result<Outputs>;

    /// The declared invocation period.
    fn period(&self) -> ExecutionDuration;

    /// The owner's lifecycle status.
    fn status(&self) -> RuntimeStatus;

    /// Marks the owner terminal after a harness-side failure.
    fn fail(&mut self);

    /// Reinitializes from the configuration's JSON document.
    fn reset(&mut self, now: ExecutionTime, config: serde_json::Value) -> crate::Result<()>;

    /// Encodes the current state's publications through the real bounded
    /// output encoding, filtered by the bootstrap rule, as field-keyed
    /// records. Encoding failures surface: bootstrap publications are
    /// bounded and admitted like any other publication.
    fn state_records(
        &self,
        context: StepContext,
        ports: &[(&'static str, Option<PortSignature>)],
        bootstrap: bool,
    ) -> crate::Result<Vec<(&'static str, Vec<u8>)>>;
}

/// Encodes one candidate state's publications through the real bounded
/// bindings, discarding the records: validation only.
fn owner_encode_check<A>(
    owner: &A,
    state: &<A as Runtime>::State,
    context: StepContext,
    ports: &[(&'static str, Option<PortSignature>)],
) -> crate::Result<()>
where
    A: RegisteredRuntime + super::outputs::OutputBindings,
{
    <A as super::outputs::OutputBindings>::encode_transport(
        owner,
        state,
        context,
        &|name| {
            ports
                .iter()
                .find(|(field, _)| *field == name)
                .and_then(|(_, signature)| *signature)
        },
        "phoxal-harness",
    )
    .map(|_| ())
}

impl<A> OwnerOps<A::Inputs, A::Outputs> for RuntimeOwner<A>
where
    A: RegisteredRuntime + super::outputs::OutputBindings + Send + Sync + 'static,
    <A as Runtime>::State: Send,
    A::Inputs: InputSet,
    A::Outputs: OutputSet,
{
    fn accept(
        &mut self,
        context: StepContext,
        inputs: &A::Inputs,
        ports: &[(&'static str, Option<PortSignature>)],
        validate: &(dyn Fn(&A::Outputs) -> crate::Result<()> + '_),
    ) -> crate::Result<A::Outputs> {
        let mut admission = HarnessAdmission {
            context,
            ports: ports.to_vec(),
            validate,
        };
        self.accept_with_hook(
            &context,
            inputs,
            &mut admission,
            |owner, state, context, _| {
                // The hook sees the candidate state before acceptance: encode
                // its projected publications through the same bounded bindings
                // the runner uses, so an oversized projection fails the
                // candidate instead of leaking through later capture.
                owner_encode_check::<A>(owner, state, *context, ports)
            },
        )
        .map(|accepted| accepted.into_outputs())
    }

    fn period(&self) -> ExecutionDuration {
        A::SPEC.period
    }

    fn status(&self) -> RuntimeStatus {
        RuntimeOwner::status(self)
    }

    fn fail(&mut self) {
        RuntimeOwner::fail(self);
    }

    fn reset(&mut self, now: ExecutionTime, config: serde_json::Value) -> crate::Result<()> {
        let config = decode_harness_config::<A>(config)?;
        RuntimeOwner::reset(self, now, config)
    }

    fn state_records(
        &self,
        context: StepContext,
        ports: &[(&'static str, Option<PortSignature>)],
        bootstrap: bool,
    ) -> crate::Result<Vec<(&'static str, Vec<u8>)>> {
        let Some(state) = self.state_ref() else {
            return crate::Result::Ok(Vec::new());
        };
        let records = <A as super::outputs::OutputBindings>::encode_transport(
            // The adapter is a unit; its reference is constructed through
            // the owner's service handle.
            self.service(),
            state,
            context,
            &|name| {
                ports
                    .iter()
                    .find(|(field, _)| *field == name)
                    .and_then(|(_, signature)| *signature)
            },
            "phoxal-harness",
        )?;
        let mut captured = Vec::new();
        for record in records {
            let Some(field) = record.field() else {
                continue;
            };
            let Some(metadata) = <A as super::outputs::OutputBindings>::FIELDS
                .iter()
                .find(|candidate| candidate.name == field)
            else {
                continue;
            };
            if metadata.kind != super::outputs::OutputKind::State {
                continue;
            }
            if bootstrap && !metadata.bootstrap {
                continue;
            }
            captured.push((field, record.payload_bytes().to_vec()));
        }
        crate::Result::Ok(captured)
    }
}

/// One owner driver for a harness: the real runtime owner behind the
/// object-safe operations, with the adapter hidden.
pub struct HarnessDriver<Inputs, Outputs> {
    owner: Box<dyn OwnerOps<Inputs, Outputs> + Send>,
}

impl<Inputs, Outputs> HarnessDriver<Inputs, Outputs> {
    fn as_ops(&mut self) -> &mut (dyn OwnerOps<Inputs, Outputs> + Send) {
        &mut *self.owner
    }
}

/// Builds one harness driver over a generated adapter from the
/// configuration's JSON document. Generated attachment code calls this;
/// authored tests never name the adapter.
pub fn new_harness_driver<A>(
    config: serde_json::Value,
) -> crate::Result<HarnessDriver<A::Inputs, A::Outputs>>
where
    A: RegisteredRuntime + super::outputs::OutputBindings + Default + Send + Sync + 'static,
    A::Inputs: InputSet,
    A::Outputs: OutputSet,
    <A as Runtime>::State: Send,
{
    let config = decode_harness_config::<A>(config)?;
    let owner = RuntimeOwner::new(A::default(), ExecutionTime::default(), config)?;
    Ok(HarnessDriver {
        owner: Box::new(owner),
    })
}

/// Decodes a configuration document into the exact runtime configuration
/// type, rejecting unknown fields like the launch path does.
fn decode_harness_config<A: RegisteredRuntime>(
    config: serde_json::Value,
) -> crate::Result<A::Config> {
    serde_json::from_value(config)
        .map_err(|error| crate::anyhow!("harness configuration is invalid: {error}"))
}

/// One explicit-time harness for a single authored runtime.
///
/// Constructed with [`Harness::new`], driven with [`Harness::advance_to`],
/// and queried through the generated endpoint methods available directly
/// on the harness value.
pub struct Harness<R: HarnessAttachment>
where
    R::Inputs: InputSet,
    R::Outputs: OutputSet,
{
    driver: HarnessDriver<R::Inputs, R::Outputs>,
    view: R::HarnessView,
    owner_id: u64,
    period: ExecutionDuration,
    next_release: ExecutionTime,
    previous_release: Option<ExecutionTime>,
    next_index: u64,
    now: ExecutionTime,
}

/// Allocates harness owner identities so a call token from one harness can
/// never consume another harness's replies.
static HARNESS_OWNERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl<R: HarnessAttachment> Harness<R>
where
    R::Inputs: InputSet,
    R::Outputs: OutputSet,
{
    /// Initializes the runtime and captures its bootstrap publication.
    pub fn new(config: impl serde::Serialize) -> crate::Result<Self> {
        let config = serde_json::to_value(config)
            .map_err(|error| crate::anyhow!("harness configuration is not encodable: {error}"))?;
        let mut driver = R::harness_driver(config)?;
        let owner_id = HARNESS_OWNERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut view = R::HarnessView::default();
        R::bind_harness(&mut view, owner_id);
        let period = driver.as_ops().period();
        let ports = input_ports::<R>();
        let context = StepContext::first(ExecutionTime::default(), period);
        for (field, bytes) in driver.as_ops().state_records(context, &ports, true)? {
            R::store_state_record(&mut view, field, bytes);
        }
        Ok(Self {
            driver,
            view,
            owner_id,
            period,
            next_release: ExecutionTime::default(),
            previous_release: None,
            next_index: 0,
            now: ExecutionTime::default(),
        })
    }

    /// Advances the explicit clock to the target instant, executing every
    /// due release of the declared cadence exactly once.
    ///
    /// Releases strictly after the target do not run, repeated advancement
    /// to the same instant executes nothing further, and a reversed target
    /// or timeline overflow is a typed error. One operation executes at
    /// most [`MAX_RELEASES_PER_ADVANCE`] releases: exhaustion preserves
    /// accepted progress and reports the boundary, and advancing again
    /// resumes the outstanding releases. A failed invocation or admission
    /// makes the harness execution terminal; later advancement neither
    /// invokes nor reserves again.
    pub fn advance_to(&mut self, target: Duration) -> Result<u64, HarnessError> {
        let target_nanos =
            u64::try_from(target.as_nanos()).map_err(|_| HarnessError::ClockOverflow)?;
        if target_nanos < self.now.as_nanos() {
            return Err(HarnessError::ClockReversed {
                requested: target_nanos,
                current: self.now.as_nanos(),
            });
        }
        self.now = ExecutionTime::from_nanos(target_nanos);
        if self.driver.as_ops().status() != RuntimeStatus::Ready {
            return Err(HarnessError::Terminal {
                source: crate::anyhow!("the runtime execution already failed"),
            });
        }
        let ports = input_ports::<R>();
        let mut executed = 0_u64;
        while self.next_release.as_nanos() <= target_nanos {
            if executed == MAX_RELEASES_PER_ADVANCE {
                return Err(HarnessError::WorkExhausted {
                    executed,
                    remaining: outstanding_releases(self.next_release, target_nanos, self.period),
                });
            }
            let context = match self.previous_release {
                None => StepContext::first(self.next_release, self.period),
                Some(previous) => StepContext::from_previous(
                    self.next_release,
                    self.period,
                    Some(previous),
                    0,
                    self.next_index,
                ),
            };
            // Preflight the post-release cursor arithmetic before
            // invoking: a release whose successor cursor is not
            // representable is refused as a clock overflow, never leaving
            // an accepted owner ahead of its release cursor or permitting
            // duplicate execution on retry.
            let next_cursor = self
                .next_release
                .as_nanos()
                .checked_add(self.period.as_nanos())
                .ok_or(HarnessError::ClockOverflow)?;
            let inputs = R::build_inputs(&mut self.view, self.next_release);
            let view = &self.view;
            let validate = move |outputs: &R::Outputs| R::validate_retention(view, outputs);
            let outputs = self
                .driver
                .as_ops()
                .accept(context, &inputs, &ports, &validate)
                .map_err(|source| HarnessError::Terminal { source })?;
            R::capture_outputs(&mut self.view, &outputs).map_err(|source| {
                self.driver.as_ops().fail();
                HarnessError::Terminal { source }
            })?;
            let records = self
                .driver
                .as_ops()
                .state_records(context, &ports, false)
                .map_err(|source| {
                    self.driver.as_ops().fail();
                    HarnessError::Terminal { source }
                })?;
            for (field, bytes) in records {
                R::store_state_record(&mut self.view, field, bytes);
            }
            self.previous_release = Some(self.next_release);
            self.next_release = ExecutionTime::from_nanos(next_cursor);
            self.next_index = self.next_index.saturating_add(1);
            executed += 1;
        }
        Ok(executed)
    }

    /// Returns the typed correlated reply for one enqueued call.
    ///
    /// The reply is consumed; a call's reply can be taken exactly once.
    pub fn reply<Response>(&mut self, call: HarnessCall<Response>) -> Result<Response, HarnessError>
    where
        Response: CallResponse,
    {
        if call.owner != self.owner_id {
            return Err(HarnessError::ForeignCall);
        }
        match R::take_reply_bytes(&mut self.view, call.id) {
            Some(bytes) => Response::decode_call_response(bytes.as_slice()).map_err(|source| {
                HarnessError::Terminal {
                    source: anyhow::Error::new(source),
                }
            }),
            None => {
                if R::call_is_staged(&self.view, call.id) {
                    // The correlation is staged for a future release: its
                    // reply has not been accepted yet.
                    Err(HarnessError::ReplyPending)
                } else if call.id.sequence() < R::call_upper_bound(&self.view) {
                    // The correlation was issued by this harness, and its
                    // reply is no longer retained: it was consumed, or its
                    // candidate was rejected before acceptance.
                    Err(HarnessError::ReplyConsumed)
                } else {
                    Err(HarnessError::ReplyPending)
                }
            }
        }
    }

    /// Reinitializes the runtime from a fresh configuration, discarding
    /// every staged input and retained effect, restarting the local
    /// release schedule at the current instant, and capturing only the
    /// fresh bootstrap publication.
    ///
    /// This is an in-memory reset: it does not create a transport
    /// execution or simulation timeline fence.
    pub fn reset(&mut self, config: impl serde::Serialize) -> crate::Result<()> {
        let config = serde_json::to_value(config)
            .map_err(|error| crate::anyhow!("harness configuration is not encodable: {error}"))?;
        self.driver.as_ops().reset(self.now, config)?;
        // The fresh bootstrap publication encodes through the bounded
        // bindings BEFORE any retained state is touched: a failed reset
        // publication is terminal — like failed initialization — and
        // leaves the previous retained effects untouched while the owner
        // retires, so no later invocation or reservation can happen.
        let ports = input_ports::<R>();
        let context = StepContext::first(self.next_release, self.period);
        let records = match self.driver.as_ops().state_records(context, &ports, true) {
            Ok(records) => records,
            Err(source) => {
                self.driver.as_ops().fail();
                return Err(source);
            }
        };
        R::clear(&mut self.view);
        for (field, bytes) in records {
            R::store_state_record(&mut self.view, field, bytes);
        }
        self.next_release = self.now;
        self.previous_release = None;
        self.next_index = 0;
        Ok(())
    }
}

/// One enqueued call correlation returned by a generated `enqueue_*`
/// method and consumed by [`Harness::reply`].
pub struct HarnessCall<Response> {
    id: CommandId,
    owner: u64,
    response: std::marker::PhantomData<fn() -> Response>,
}

impl<Response> Clone for HarnessCall<Response> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            owner: self.owner,
            response: std::marker::PhantomData,
        }
    }
}

impl<Response> HarnessCall<Response> {
    /// Builds a call correlation bound to one harness owner.
    #[must_use]
    pub fn new(id: CommandId, owner: u64) -> Self {
        Self {
            id,
            owner,
            response: std::marker::PhantomData,
        }
    }
}

/// The explicit-time harness's typed failure surface.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    /// The requested instant precedes the harness's current time.
    #[error("harness clock cannot move backwards: requested {requested} ns, current {current} ns")]
    ClockReversed {
        /// The rejected target instant.
        requested: u64,
        /// The harness's current instant.
        current: u64,
    },
    /// The target instant is not representable on the execution timeline.
    #[error("harness clock target overflows the execution timeline")]
    ClockOverflow,
    /// The per-operation release budget ran out; accepted progress is
    /// preserved and the caller resumes by advancing again.
    #[error(
        "harness executed {executed} releases and {remaining} remain to the target; advance again to resume"
    )]
    WorkExhausted {
        /// Releases executed by the exhausted operation.
        executed: u64,
        /// Releases still outstanding to the requested target.
        remaining: u64,
    },
    /// The runtime execution failed (invocation or admission) and is
    /// terminal for this harness.
    #[error("harness runtime execution is terminal: {source}")]
    Terminal {
        /// The originating failure.
        source: anyhow::Error,
    },
    /// Retained undrained effects exceeded the contract's bounds; drain
    /// the retained storage before advancing further.
    #[error("retained harness effects exceeded their declared bounds; drain them before advancing")]
    RetainedFull,
    /// A staged input queue reached its endpoint's declared item or byte
    /// bound.
    #[error("pending input queue for `{endpoint}` is at its declared bound")]
    PendingFull {
        /// The endpoint whose staged queue is full.
        endpoint: &'static str,
    },
    /// A staged input could not be encoded against its contract bounds.
    #[error("pending input for `{endpoint}` could not be encoded")]
    PendingUnencodable {
        /// The endpoint whose staged item failed to encode.
        endpoint: &'static str,
    },
    /// The call's reply has not been accepted yet.
    #[error("the call's reply has not been accepted yet")]
    ReplyPending,
    /// The call token was issued by a different harness.
    #[error("the call token was issued by a different harness")]
    ForeignCall,
    /// The call's reply was already consumed.
    #[error("the call's reply was already consumed")]
    ReplyConsumed,
}

fn outstanding_releases(next: ExecutionTime, target: u64, period: ExecutionDuration) -> u64 {
    if next.as_nanos() > target {
        return 0;
    }
    let span = target - next.as_nanos();
    let period_nanos = period.as_nanos().max(1);
    (span / period_nanos) + 1
}

fn input_ports<R: HarnessAttachment>() -> Vec<(&'static str, Option<PortSignature>)>
where
    R::Inputs: InputSet,
    R::Outputs: OutputSet,
{
    <<R as EndpointView>::Inputs as InputSet>::FIELDS
        .iter()
        .map(|field| (field.name, field.port_signature))
        .collect()
}

/// The harness's admission: reserves by encoding the complete candidate
/// batch through the real output encoding path and validating the
/// candidate's complete retained effect against every endpoint's declared
/// bounds, exactly as the transport boundary does between dispatch and
/// publication.
struct HarnessAdmission<'a, O> {
    context: StepContext,
    ports: Vec<(&'static str, Option<PortSignature>)>,
    validate: &'a (dyn Fn(&O) -> crate::Result<()> + 'a),
}

impl<O: OutputSet> OutputAdmission<O> for HarnessAdmission<'_, O> {
    type Reservation = ();

    fn reserve(&mut self, outputs: &O) -> crate::Result<Self::Reservation> {
        outputs.encode_transport(
            self.context,
            &|name| {
                self.ports
                    .iter()
                    .find(|(field, _)| *field == name)
                    .and_then(|(_, signature)| *signature)
            },
            "phoxal-harness",
        )?;
        (self.validate)(outputs)
    }
}

impl<R: HarnessAttachment> Deref for Harness<R>
where
    R::Inputs: InputSet,
    R::Outputs: OutputSet,
{
    type Target = R::HarnessView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl<R: HarnessAttachment> DerefMut for Harness<R>
where
    R::Inputs: InputSet,
    R::Outputs: OutputSet,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.view
    }
}
