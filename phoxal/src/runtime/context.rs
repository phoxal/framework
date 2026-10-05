//! The common invocation context handed to runtime handlers and steps.
//!
//! One [`Context`] borrows this invocation's frozen inputs, its staged output
//! transaction, and the common invocation facts. It never borrows the runtime
//! value itself, so an authored runtime can hold a behavior tree in an
//! ordinary field and tick it as `self.mission.tick(ctx)` while its own
//! `&mut self` borrow is live.
//!
//! Endpoint-specific methods (`ctx.encoder()`, `ctx.emit_finished(...)`,
//! `ctx.start_countdown(...)`) are not defined here: Rust forbids downstream
//! inherent methods on an SDK-owned type. The endpoint expansion generates a
//! local view for each runtime type and this context exposes it through
//! [`Deref`] and [`DerefMut`], so handlers reach every endpoint method
//! directly on the context without importing an extension trait.

use std::ops::{Deref, DerefMut};

use super::ExecutionDuration;
use super::Sample;
use super::StepContext;
use super::core::ExecutionTime;
use super::input::{Latest, Setpoint};

/// Associates one authored runtime type with the endpoint view generated
/// from its contract.
///
/// The runtime attachment implements this for the authored type; it is not a
/// hand-authoring surface.
pub trait EndpointView {
    /// The runtime's frozen input transaction type.
    type Inputs;
    /// The runtime's staged output transaction type.
    type Outputs;
    /// The generated endpoint view over one invocation.
    type View<'a>
    where
        Self: 'a;

    /// Builds the endpoint view over one invocation's frozen inputs and
    /// staged outputs.
    fn endpoint_view<'a>(
        step: &'a StepContext,
        inputs: &'a Self::Inputs,
        outputs: &'a mut Self::Outputs,
    ) -> Self::View<'a>;

    /// Takes one completion by ticket from the endpoint view's completion
    /// inputs, destructively. Contracts without generated calls complete
    /// nothing.
    fn take_completion_bytes(
        _view: &mut Self::View<'_>,
        _ticket: u128,
    ) -> Option<Result<Vec<u8>, super::input::RequestError>> {
        None
    }

    /// Retires one completion's local ownership without consuming it.
    fn retire_completion_bytes(_view: &mut Self::View<'_>, _ticket: u128) {}

    /// Stages one generated operation through the endpoint view without
    /// direct-field ownership: the ticket's completion stays with its
    /// staging site (a behavior leaf), never with a direct completion
    /// handler. Contracts without generated operations refuse.
    fn stage_tree_operation<O>(
        _view: &mut Self::View<'_>,
        _generation: u64,
        _operation: O,
    ) -> crate::Result<super::outputs::CallTicket<O::Response>>
    where
        O: super::outputs::GeneratedSend,
    {
        Err(crate::anyhow!(
            "this runtime's contract stages no generated operations"
        ))
    }
}

/// Adapter-owned lifecycle resources shared by every authored entry point.
/// Generated dispatch uses this bundle so handlers, completions, and steps
/// all receive the same ownership ledgers and diagnostic staging boundary.
#[derive(Clone, Copy)]
pub struct ContextResources<'a> {
    /// The adapter's pending-call ownership ledger.
    pub pending: &'a super::pending::PendingCalls,
    /// The adapter's admitted-event capture registry.
    pub captures: &'a super::capture::CaptureRegistry,
    /// The adapter's candidate diagnostic diary.
    pub diary: &'a super::behavior::diary::BehaviorDiary,
}

impl ContextResources<'_> {
    /// Builds one complete context for an authored entry point.
    pub fn context<'a, R: EndpointView + ?Sized + 'a>(
        &'a self,
        step: &'a StepContext,
        inputs: &'a R::Inputs,
        outputs: &'a mut R::Outputs,
    ) -> Context<'a, R> {
        Context::<R>::with_pending(step, inputs, outputs, Some(self.pending))
            .with_captures(self.captures)
            .with_diary(self.diary)
    }
}

/// One runtime invocation's frozen inputs, staged outputs, and common facts.
///
/// Handlers and steps receive `&mut Context<'_, Self>`; publishers never
/// receive a context because they project from state alone.
pub struct Context<'a, R: EndpointView + ?Sized + 'a> {
    step: &'a StepContext,
    view: R::View<'a>,
    pending: Option<&'a super::pending::PendingCalls>,
    /// The construction generation of the tree currently ticking through
    /// this context; zero when no tree is ticking. Trees set it around
    /// their tick and cancel so ownership is scoped to the concrete
    /// consumer.
    tree_generation: std::cell::Cell<u64>,
    captures: Option<&'a super::capture::CaptureRegistry>,
    /// The authenticated source of the admitted item the dispatch driver is
    /// currently delivering, when one is being delivered.
    dispatch_source: Option<&'a str>,
    /// The adapter-owned accepted-behavior diary trees stage their
    /// compact per-tick records into.
    diary: Option<&'a super::behavior::diary::BehaviorDiary>,
    /// Evidence that a local retirement abandoned one or more outstanding
    /// tree-owned calls this tick: local cleanup is not proof that the
    /// remote effects stopped, so the halting composite must report an
    /// uncertain outcome instead of a domain-safe refusal.
    abandoned_effects: std::cell::Cell<bool>,
}

impl<'a, R: EndpointView + ?Sized + 'a> Context<'a, R> {
    /// Builds the context over one invocation's frozen inputs and staged
    /// outputs.
    ///
    /// This constructor serves the generated runtime adapter and dispatch
    /// glue; authored code receives the context in a handler or step.
    pub fn new(step: &'a StepContext, inputs: &'a R::Inputs, outputs: &'a mut R::Outputs) -> Self {
        Self::with_pending(step, inputs, outputs, None)
    }

    /// Builds the context the runtime adapter hands to its steps and
    /// handlers: it also names the runtime's pending-call ledger, so
    /// tree-owned completions are delivered only to the leaf whose
    /// accepted call the ledger recorded, and retired calls lose their
    /// future eligibility. Hand-built contexts carry no ledger and keep
    /// raw single-store semantics.
    pub fn with_pending(
        step: &'a StepContext,
        inputs: &'a R::Inputs,
        outputs: &'a mut R::Outputs,
        pending: Option<&'a super::pending::PendingCalls>,
    ) -> Self {
        Self {
            step,
            view: R::endpoint_view(step, inputs, outputs),
            pending,
            tree_generation: std::cell::Cell::new(0),
            captures: None,
            dispatch_source: None,
            diary: None,
            abandoned_effects: std::cell::Cell::new(false),
        }
    }

    /// Names the authenticated source of the admitted item being delivered
    /// by the dispatch driver. Set by the generated dispatch around each
    /// handler call; a step or tree tick observes no dispatch source.
    pub fn with_dispatch_source(mut self, source: Option<&'a str>) -> Self {
        self.dispatch_source = source;
        self
    }

    /// The authenticated caller identity of the admitted command the
    /// dispatch driver is delivering to this handler, when one is.
    #[must_use]
    pub fn command_source(&self) -> Option<&str> {
        self.dispatch_source
    }

    /// Names the runtime's capture registry so behavior nodes can
    /// activate, poll, and release typed captures of already-admitted
    /// input. Hand-built contexts carry no registry and observe nothing.
    pub fn with_captures(mut self, captures: &'a super::capture::CaptureRegistry) -> Self {
        self.captures = Some(captures);
        self
    }

    /// Names the adapter-owned accepted-behavior diary so owned trees
    /// stage their compact per-tick records with the invocation
    /// candidate. Hand-built contexts carry no diary and record nothing.
    pub fn with_diary(mut self, diary: &'a super::behavior::diary::BehaviorDiary) -> Self {
        self.diary = Some(diary);
        self
    }

    /// The adapter-owned accepted-behavior diary this context can reach,
    /// when the adapter supplied one.
    #[must_use]
    pub fn behavior_diary(&self) -> Option<&'a super::behavior::diary::BehaviorDiary> {
        self.diary
    }

    /// The adapter's outstanding generated calls: the bounded pending
    /// ledger's size, or zero for hand-built contexts.
    #[must_use]
    pub fn pending_calls_len(&self) -> usize {
        self.pending.map_or(0, super::pending::PendingCalls::len)
    }

    /// The adapter's active typed captures.
    #[must_use]
    pub fn active_captures(&self) -> usize {
        self.captures
            .map_or(0, super::capture::CaptureRegistry::active_count)
    }

    /// The capture registry this context can reach, when the adapter
    /// supplied one.
    #[must_use]
    pub fn captures(&self) -> Option<&'a super::capture::CaptureRegistry> {
        self.captures
    }

    /// Names the tree generation whose leaves are ticking through this
    /// context. Set by [`crate::runtime::behavior::Tree`] around each tick
    /// and cancellation; staged tree calls and consumed completions are
    /// scoped to this concrete generation.
    pub fn enter_tree(&self, generation: u64) {
        self.tree_generation.set(generation);
    }

    /// The tree generation currently owning staged calls through this
    /// context; zero when no tree is ticking.
    #[must_use]
    pub fn current_tree_generation(&self) -> u64 {
        self.tree_generation.get()
    }

    /// The execution time of this invocation.
    #[must_use]
    pub fn now(&self) -> ExecutionTime {
        self.step.now()
    }

    /// The runtime's declared invocation period.
    #[must_use]
    pub fn period(&self) -> ExecutionDuration {
        self.step.period()
    }

    /// The execution time elapsed since the previous accepted invocation.
    #[must_use]
    pub fn elapsed(&self) -> ExecutionDuration {
        self.step.elapsed()
    }

    /// Releases the schedule skipped before this invocation.
    #[must_use]
    pub fn missed_releases(&self) -> u64 {
        self.step.missed_releases()
    }

    /// The zero-based index of this accepted invocation.
    #[must_use]
    pub fn invocation_index(&self) -> u64 {
        self.step.invocation_index()
    }

    /// Takes the typed completion of one submitted call, exclusively: the
    /// ticket's result is consumed and never delivered again. Under an
    /// adapter-owned ledger the ticket must be a committed tree-owned
    /// call; foreign, retired, staged, or direct-owned tickets are never
    /// delivered. A response that fails decoding carries
    /// [`super::input::RequestError::Integrity`] so consumers fault
    /// rather than treat it as a domain outcome.
    pub fn take_completion<Response>(
        &mut self,
        ticket: &super::outputs::CallTicket<Response>,
    ) -> Option<super::input::CallCompletion<Response>>
    where
        Response: super::input::CallResponse,
    {
        // Ownership is spent only when a result is actually taken: a leaf
        // may poll cuts that do not yet hold its reply, and polling must
        // never consume the call's eligibility.
        if let Some(pending) = self.pending {
            let generation = self.tree_generation.get();
            if !pending.is_tree_call(ticket.id(), generation) {
                return None;
            }
            let raw = R::take_completion_bytes(&mut self.view, ticket.id())?;
            pending.consume_tree(ticket.id());
            return super::input::CallCompletion::from_transport_result(ticket.id(), raw).ok();
        }
        let raw = R::take_completion_bytes(&mut self.view, ticket.id())?;
        super::input::CallCompletion::from_transport_result(ticket.id(), raw).ok()
    }

    /// Stages one generated call on this invocation's output transaction
    /// without direct-field ownership: the caller keeps the returned ticket
    /// and consumes the completion itself, so no direct completion handler
    /// ever receives it.
    pub fn send<O>(
        &mut self,
        operation: O,
    ) -> crate::Result<super::outputs::CallTicket<O::Response>>
    where
        O: super::outputs::GeneratedSend,
    {
        R::stage_tree_operation(&mut self.view, self.tree_generation.get(), operation)
    }

    /// Retires one call's local completion ownership without consuming a
    /// result, releasing the retained mailbox slot.
    pub fn retire_completion<Response>(&mut self, ticket: &super::outputs::CallTicket<Response>) {
        if let Some(pending) = self.pending {
            pending.retire(ticket.id());
        }
        R::retire_completion_bytes(&mut self.view, ticket.id());
    }

    /// Whether one staged call's remote effect may have executed: under a
    /// pending ledger only a still-committed call is outstanding; without
    /// a ledger the leaf's own unconsumed ticket is the best evidence.
    pub(in crate::runtime) fn call_may_have_executed<Response>(
        &self,
        ticket: &super::outputs::CallTicket<Response>,
    ) -> bool {
        match self.pending {
            Some(pending) => pending.is_any_tree_call(ticket.id()),
            None => true,
        }
    }

    /// Records that a local retirement abandoned one outstanding call:
    /// local cleanup is not proof that the remote effect stopped, so the
    /// halting composite must report an uncertain outcome.
    pub(in crate::runtime) fn note_abandoned_effect(&self) {
        self.abandoned_effects.set(true);
    }

    /// Takes this tick's abandoned-effect evidence: returns whether a
    /// local retirement abandoned an outstanding call since the last take.
    pub(in crate::runtime) fn take_abandoned_effects(&self) -> bool {
        self.abandoned_effects.replace(false)
    }

    /// Clears the abandoned-effect evidence at a tick boundary so stale
    /// evidence from an earlier tick cannot leak into a later decision.
    pub(in crate::runtime) fn reset_abandoned_effects(&self) {
        self.abandoned_effects.set(false);
    }
}

impl<'a, R: EndpointView + ?Sized + 'a> Deref for Context<'a, R> {
    type Target = R::View<'a>;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl<'a, R: EndpointView + ?Sized + 'a> DerefMut for Context<'a, R> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.view
    }
}

/// Read side of one latest-observation endpoint, carrying the endpoint's
/// declared age bound.
///
/// Created by the generated endpoint view; reading it never renews the
/// original capture evidence. Borrowed results are tied to the frozen input
/// cut's lifetime, never to this wrapper, so `ctx.<endpoint>().fresh()` can
/// be bound or consumed inline while the context is used again afterward.
pub struct Observation<'a, T> {
    latest: &'a Latest<T>,
    now: ExecutionTime,
    max_age_ms: Option<u64>,
}

impl<'a, T> Observation<'a, T> {
    /// Builds the observation read over one latest input.
    #[must_use]
    pub fn new(latest: &'a Latest<T>, now: ExecutionTime, max_age_ms: Option<u64>) -> Self {
        Self {
            latest,
            now,
            max_age_ms,
        }
    }

    /// Whether the admitted graph supplies this input, including while unavailable.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.latest.is_connected()
    }

    /// Returns the payload when it satisfies the endpoint's declared age
    /// bound at this invocation's time.
    #[must_use]
    pub fn fresh(&self) -> Option<&'a T> {
        self.latest
            .sample()
            .filter(|_| self.latest.is_fresh_at(self.now, self.max_age_ms))
            .map(|sample| sample.payload())
    }

    /// Returns the payload when it satisfies an explicitly caller-owned age
    /// bound at this invocation's time, under the same latest-retention
    /// skew policy as the declared bound. Services whose freshness window
    /// comes from configuration read through this instead of the
    /// contract-declared bound.
    #[must_use]
    pub fn fresh_within(&self, max_age_ms: u64) -> Option<&'a T> {
        self.latest
            .sample()
            .filter(|_| self.latest.is_fresh_at(self.now, Some(max_age_ms)))
            .map(|sample| sample.payload())
    }

    /// Reports whether the payload satisfies the endpoint's age bound.
    #[must_use]
    pub fn is_fresh(&self) -> bool {
        self.latest.is_fresh_at(self.now, self.max_age_ms)
    }

    /// Returns the payload regardless of age, when one was admitted.
    #[must_use]
    pub fn value(&self) -> Option<&'a T> {
        self.latest.value()
    }

    /// Returns the captured sample with its original stamp, when admitted.
    #[must_use]
    pub fn sample(&self) -> Option<&'a Sample<T>> {
        self.latest.sample()
    }
}

/// Read side of one leased latest endpoint.
///
/// Created by the generated endpoint view; the lease's authority and expiry
/// stay owned by the producer. Borrowed results are tied to the frozen input
/// cut's lifetime, never to this wrapper.
pub struct Leased<'a, T> {
    setpoint: &'a Setpoint<T>,
    now: ExecutionTime,
}

impl<'a, T> Leased<'a, T> {
    /// Builds the leased read over one setpoint input.
    #[must_use]
    pub fn new(setpoint: &'a Setpoint<T>, now: ExecutionTime) -> Self {
        Self { setpoint, now }
    }

    /// Returns the value while its lease is valid at this invocation's time.
    #[must_use]
    pub fn valid(&self) -> Option<&'a T> {
        self.setpoint
            .value()
            .filter(|_| self.setpoint.is_valid_at(self.now))
    }

    /// Returns the value regardless of lease validity, when present.
    #[must_use]
    pub fn value(&self) -> Option<&'a T> {
        self.setpoint.value()
    }

    /// Returns the authenticated owner identity for the current value.
    #[must_use]
    pub fn source(&self) -> Option<&'a str> {
        self.setpoint.source()
    }

    /// Returns the issue instant of the current intent.
    #[must_use]
    pub fn issued_at(&self) -> Option<ExecutionTime> {
        self.setpoint.issued_at()
    }
}
