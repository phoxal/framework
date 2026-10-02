//! Step-driven sequences and behavior trees over one runtime.
//!
//! Every node is driven by ordinary runtime steps against the frozen input
//! cut through [`crate::runtime::Context`]; there is no second scheduler,
//! thread, or blackboard. Trees are owned values stored in ordinary
//! authored fields, so `self.mission.tick(ctx)` works while the step holds
//! `&mut self`: the context never borrows the runtime.
//!
//! The fluent [`Sequence`] lowers to the same public [`Node`] composition
//! surface as explicit [`selector`]/[`guard`]/[`all`]/[`race`] trees:
//! composites remember their active children, successful effects never
//! replay, halts retire owned subtrees exactly once, and a selector may
//! try its next branch only after an explicitly fallback-eligible failure
//! (see [`FailureKind`]).
//!
//! The engine is a private bounded implementation. The previously
//! evaluated `bonsai-bt` candidate was rejected on evidence: its reactive
//! `While` form re-enters its body within a single tick without any
//! internal visit bound (the wave-01 probe hung in exactly that
//! traversal), and its floating-point wait accounting conflicts with
//! execution-timestamp authority. This engine bounds visits per tick, uses
//! execution timestamps for all timing, and latches terminal roots.

mod capture_node;
pub mod diary;
mod leaf;
mod node;
mod outcome;

use std::time::Duration;

use crate::contracts::Call;
use crate::runtime::context::Context;
use crate::runtime::input::CallResponse;
use outcome::Step;
use prost::Message;

pub use capture_node::{observe, wait_event};
pub use diary::{BehaviorDiary, BehaviorRecord, MAX_DIARY_BYTES, MAX_DIARY_RECORDS};
pub use leaf::{ActionHalt, ActionOutcome, ActionTick, Predicate};
pub use node::Node;
use node::{action_node, call_node, continuation_node};
pub use outcome::FailureKind;

/// Upper bound of node visits in one `tick`.
pub const MAX_VISITS_PER_TICK: usize = 10_000;
/// Upper bound of nodes in one constructed tree.
pub const MAX_NODES: usize = 1_000;
/// Upper bound of construction depth in one constructed tree.
pub const MAX_DEPTH: usize = 64;

/// The terminal or in-progress state of one tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeStatus {
    /// The tree has not reached a terminal state.
    Running,
    /// Every stage succeeded, including each call's accepted response
    /// predicate.
    Succeeded,
    /// An expected domain refusal or predicate mismatch ended the tree.
    Refused,
    /// A runtime integrity or transport-decode failure ended the tree.
    Failed,
    /// Local cancellation ended the tree.
    Cancelled,
    /// A deadline expired or the remote outcome stayed unknown.
    TimedOut,
}

/// One owned behavior tree.
///
/// Stored in an ordinary authored field and ticked from a step; the tree
/// owns its call tickets and remembers its progress across invocations.
/// A terminal root stays terminal until explicitly replaced with a fresh
/// tree: later ticks neither replay effects nor resubmit calls.
pub struct Tree<R: crate::runtime::EndpointView + ?Sized + 'static> {
    root: Node<R>,
    status: TreeStatus,
    ended_cause: Option<String>,
    /// The typed classification of the terminal outcome, retained for
    /// diagnostics instead of relying on cause strings.
    ended_kind: Option<FailureKind>,
    /// Whether the root's resources — captures, owned call tickets,
    /// dynamic-child allowances — have been released. Terminal ticks and
    /// cancellation retire exactly once; applications never clean up a
    /// terminal tree by hand.
    retired: bool,
    /// The remaining aggregate allowance for dynamically produced nodes
    /// (continuations and repeats), reclaimed when they retire.
    dynamic_budget: usize,
    /// The globally unique construction generation of this tree: pending
    /// calls it stages and completions it polls are scoped to this
    /// concrete consumer, so no other tree instance — and no re-created
    /// successor — can claim them.
    generation: u64,
}

/// Allocates the next globally distinct tree construction generation.
fn next_tree_generation() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static GENERATION: AtomicU64 = AtomicU64::new(1);
    GENERATION.fetch_add(1, Ordering::Relaxed)
}

impl<R: crate::runtime::EndpointView + 'static> Tree<R> {
    /// Builds one tree from a root node, validating the aggregate node
    /// and depth bounds.
    pub(super) fn from_node(root: Node<R>) -> crate::Result<Self> {
        let (size, _depth) = node::measure_bounded(&root, MAX_NODES, MAX_DEPTH, "behavior tree")?;
        let dynamic_budget = MAX_NODES - size;
        crate::Result::Ok(Self {
            root,
            status: TreeStatus::Running,
            ended_cause: None,
            ended_kind: None,
            retired: false,
            dynamic_budget,
            generation: next_tree_generation(),
        })
    }

    /// Ticks the tree once against this invocation's frozen cut.
    ///
    /// Expected behavior outcomes are retained in [`Tree::status`];
    /// `Err` is reserved for runtime integrity failures (visit budget,
    /// staging failure, or decode failure), which fault the invocation.
    pub fn tick(&mut self, context: &mut Context<'_, R>) -> crate::Result<()> {
        if self.status != TreeStatus::Running {
            return crate::Result::Ok(());
        }
        // Scope this tick's staged calls and polled completions to this
        // concrete tree, and restore the surrounding owner afterwards so
        // unrelated authored work — including a later manual `ctx.send` —
        // never inherits this tree's ownership.
        let previous = context.current_tree_generation();
        context.enter_tree(self.generation);
        context.reset_abandoned_effects();
        let outcome = {
            let mut visits = 0_usize;
            let mut budget = self.dynamic_budget;
            let step = self.root.tick(context, &mut visits, &mut budget, 0);
            self.dynamic_budget = budget;
            step
        };
        // A terminal root releases every branch resource exactly once —
        // captures activated by earlier children, unconsumed call
        // tickets, and dynamic-child allowances — inside this tree's
        // ownership scope. Applications never clean up a terminal tree
        // by hand, and cancellation of a terminal tree is a no-op.
        let outcome = outcome.map(|step| {
            if !matches!(step, Step::Running) && !self.retired {
                let reclaimed = self.root.retire(context);
                self.retired = true;
                self.dynamic_budget += reclaimed;
            }
            step
        });
        context.enter_tree(previous);
        match outcome? {
            Step::Succeeded | Step::SucceededWith(_) => self.status = TreeStatus::Succeeded,
            Step::Running => {}
            Step::Ended {
                status,
                cause,
                kind,
            } => {
                if self.ended_cause.is_none() {
                    self.ended_cause = cause;
                }
                if self.ended_kind.is_none() {
                    self.ended_kind = Some(kind);
                }
                self.status = status;
            }
        }
        // Stage this tick's compact diagnostic with the candidate.
        self.stage_diagnostic(context);
        crate::Result::Ok(())
    }

    /// Cancels the tree: local stages stop, owned completion tickets are
    /// retired, and the tree stays terminal. Idempotent. Cancelling
    /// proves nothing about a request already accepted by another
    /// participant; domain cancellation is the application's own call.
    ///
    /// The cancellation stages its terminal record with the same
    /// invocation candidate as the local retirement: it supersedes this
    /// tree's earlier running snapshot from the same invocation, and is
    /// published exactly when that candidate is accepted.
    pub fn cancel(&mut self, context: &mut Context<'_, R>) -> crate::Result<()> {
        if self.status == TreeStatus::Running {
            let previous = context.current_tree_generation();
            context.enter_tree(self.generation);
            let reclaimed = self.root.retire(context);
            self.dynamic_budget += reclaimed;
            self.retired = true;
            context.enter_tree(previous);
            self.ended_cause = Some("the tree was cancelled locally".to_owned());
            self.ended_kind = Some(FailureKind::None);
            self.status = TreeStatus::Cancelled;
            self.stage_diagnostic(context);
        }
        crate::Result::Ok(())
    }

    /// Stages this tree's compact diagnostic with the invocation
    /// candidate: generation, invocation identity, execution time, status,
    /// typed classification, structural path, and bounded ownership
    /// counts. The record becomes observable only when the owning
    /// boundary accepts the candidate; a rejected or faulted candidate
    /// never publishes. Re-staging the same generation within one
    /// candidate supersedes that tree's own earlier snapshot.
    fn stage_diagnostic(&self, context: &Context<'_, R>) {
        if let Some(diary) = context.behavior_diary() {
            diary.stage(BehaviorRecord {
                generation: self.generation,
                invocation: context.invocation_index(),
                time_ns: context.now().as_nanos(),
                status: self.status,
                kind: self.ended_kind.unwrap_or(FailureKind::None),
                path: self.active_path(),
                pending_calls: context.pending_calls_len(),
                active_captures: context.active_captures(),
            });
        }
    }

    /// The tree's current status.
    #[must_use]
    pub const fn status(&self) -> TreeStatus {
        self.status
    }

    /// The retained cause of the terminal status, when one was recorded.
    #[must_use]
    pub fn ended_cause(&self) -> Option<&str> {
        self.ended_cause.as_deref()
    }

    /// The typed failure classification of the terminal outcome, retained
    /// for diagnostics: uncertain-effect evidence is available here rather
    /// only in cause strings.
    #[must_use]
    pub fn ended_kind(&self) -> Option<FailureKind> {
        self.ended_kind
    }

    /// One bounded diagnostic of the tree's active path: the composite
    /// structural path of the currently active node — same-kind siblings
    /// carry their child index (`sequence[1].delay`), repeat attempts
    /// carry their attempt identity, and a typed continuation's live
    /// child is visible (`call.then(...)`) — over the structure that
    /// remains, including after a terminal tick or cancellation. Bounded
    /// by the depth limit; it never copies domain payloads and never
    /// degrades into an arbitrary terminal reason string: terminal
    /// identity lives in [`Tree::status`], [`Tree::ended_kind`], and
    /// [`Tree::ended_cause`]. The same value flows through the runtime's
    /// accepted behavior diary on every accepted tick.
    #[must_use]
    pub fn active_path(&self) -> String {
        let mut path = String::new();
        // The path walk reads only structure; the context is unused.
        self.root.path(&mut path);
        path
    }
}

/// Builds a node that succeeds or stays running while the predicate does
/// not hold.
pub fn wait_until<R, F>(predicate: F) -> Node<R>
where
    R: crate::runtime::EndpointView + 'static,
    F: Fn(&Context<'_, R>) -> bool + Send + 'static,
{
    Node(node::NodeKind::WaitUntil(Box::new(predicate)))
}

/// Builds a node that waits from its own first activation for the given
/// duration, using execution timestamps.
pub fn delay<R>(duration: Duration) -> Node<R>
where
    R: crate::runtime::EndpointView + 'static,
{
    Node(node::NodeKind::Delay {
        duration,
        deadline: None,
    })
}

/// Builds a node that succeeds or fails immediately from one predicate.
pub fn condition<R, F>(predicate: F) -> Node<R>
where
    R: crate::runtime::EndpointView + 'static,
    F: Fn(&Context<'_, R>) -> bool + Send + 'static,
{
    Node(node::NodeKind::Condition(Box::new(predicate)))
}

/// Builds a sequence over ordered nodes: each child runs after the prior
/// one succeeds, exactly like the fluent [`Sequence`].
pub fn sequence<R>(children: impl IntoIterator<Item = Node<R>>) -> Node<R>
where
    R: crate::runtime::EndpointView + 'static,
{
    Node(node::NodeKind::Sequence {
        children: children.into_iter().collect(),
        active: 0,
    })
}

/// Builds a selector over ordered branches: the first branch that succeeds
/// wins; the next branch is tried only after the selected branch ends with
/// a fallback-eligible failure (expected domain refusal or a rejection
/// before admission). An uncertain remote effect stops the tree.
pub fn selector<R>(children: impl IntoIterator<Item = Node<R>>) -> Node<R>
where
    R: crate::runtime::EndpointView + 'static,
{
    Node(node::NodeKind::Selector {
        children: children.into_iter().collect(),
        active: 0,
    })
}

/// Builds a guard that rechecks its predicate before each child tick and
/// halts the child exactly once when the predicate stops holding.
pub fn guard<R, F>(predicate: F, child: Node<R>) -> Node<R>
where
    R: crate::runtime::EndpointView + 'static,
    F: Fn(&Context<'_, R>) -> bool + Send + 'static,
{
    Node(node::NodeKind::Guard {
        predicate: Box::new(predicate),
        child: Some(Box::new(child)),
        halted: false,
    })
}

/// Builds a cooperative parallel node: children tick in declaration order
/// against one frozen snapshot; the first failing child halts the
/// remaining active children, and the node succeeds once every child has.
pub fn all<R>(children: impl IntoIterator<Item = Node<R>>) -> Node<R>
where
    R: crate::runtime::EndpointView + 'static,
{
    let children: Vec<_> = children.into_iter().collect();
    let done = vec![false; children.len()];
    Node(node::NodeKind::All { children, done })
}

/// Builds a race: children tick in declaration order and the first
/// terminal child wins; every losing branch is halted locally.
pub fn race<R>(children: impl IntoIterator<Item = Node<R>>) -> Node<R>
where
    R: crate::runtime::EndpointView + 'static,
{
    let children: Vec<_> = children.into_iter().collect();
    let done = vec![false; children.len()];
    Node(node::NodeKind::Race { children, done })
}

/// Builds a finite explicit repetition. A fresh child is created only
/// after the previous one succeeds, with the zero-based attempt index
/// handed to the application factory, and no earlier than the next
/// invocation. Refusal, uncertainty, and cancellation end the repeat
/// rather than retrying.
pub fn repeat<R, F>(count: u32, factory: F) -> Node<R>
where
    R: crate::runtime::EndpointView + 'static,
    F: Fn(u32) -> crate::Result<Node<R>> + Send + 'static,
{
    Node(node::NodeKind::Repeat {
        count,
        attempt: 0,
        factory: Box::new(factory),
        child: None,
        resume_at: None,
        charged: 0,
    })
}

/// One custom action under construction: incremental bounded work with an
/// optional action-local halt hook.
pub struct ActionStage<R: crate::runtime::EndpointView + ?Sized + 'static> {
    tick: ActionTick<R>,
    halt: Option<ActionHalt<R>>,
}

/// Builds one custom action node from its incremental tick closure. The
/// closure performs bounded work per invocation and reports running,
/// success, or its own domain failure; it never blocks and never observes
/// another node's newly published output within the same cut.
pub fn action<R, F>(tick: F) -> ActionStage<R>
where
    R: crate::runtime::EndpointView + 'static,
    F: FnMut(&mut Context<'_, R>) -> crate::Result<ActionOutcome> + Send + 'static,
{
    ActionStage {
        tick: Box::new(tick),
        halt: None,
    }
}

impl<R: crate::runtime::EndpointView + 'static> ActionStage<R> {
    /// Attaches one action-local halt hook, run exactly once when the
    /// branch is retired or the action ends.
    #[must_use]
    pub fn with_halt<F>(mut self, halt: F) -> Self
    where
        F: FnOnce(&mut Context<'_, R>) + Send + 'static,
    {
        self.halt = Some(Box::new(halt));
        self
    }

    /// Finishes the action node.
    #[must_use]
    pub fn into_node(self) -> Node<R> {
        action_node(self.tick, self.halt)
    }
}

/// The fluent sequence builder. Lowers to the same [`Node`] engine as
/// explicit composition.
pub struct Sequence<R: crate::runtime::EndpointView + ?Sized + 'static> {
    stages: Vec<Node<R>>,
    budget: Option<Duration>,
}

impl<R: crate::runtime::EndpointView + 'static> Sequence<R> {
    /// Starts one empty sequence for the given runtime type.
    #[must_use]
    pub fn new() -> Self {
        Self {
            stages: Vec::new(),
            budget: None,
        }
    }

    /// Adds a stage that waits until the predicate holds on the frozen
    /// cut.
    #[must_use]
    pub fn wait_until(
        mut self,
        predicate: impl Fn(&Context<'_, R>) -> bool + Send + 'static,
    ) -> Self {
        self.stages.push(wait_until(predicate));
        self
    }

    /// Adds a call stage from one inert call (typically a generated
    /// `api::calls::<endpoint>(request)` constructor); the tree stages and
    /// owns the call itself, so its completion never reaches a direct
    /// completion handler. The stage is complete once its expected-response
    /// predicate is declared.
    #[must_use]
    pub fn call<Request, Response>(
        self,
        call: Call<Request, Response>,
    ) -> CallStage<R, Request, Response>
    where
        Request: Message + Send + 'static,
        Response: CallResponse + 'static,
    {
        CallStage {
            stages: self.stages,
            budget: self.budget,
            call: Some(call),
        }
    }

    /// Adds a stage that waits from its own first activation.
    #[must_use]
    pub fn delay(mut self, duration: Duration) -> Self {
        self.stages.push(Node(node::NodeKind::Delay {
            duration,
            deadline: None,
        }));
        self
    }

    /// Bounds the composed tree from its first admitted tick.
    #[must_use]
    pub fn within(mut self, budget: Duration) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Lowers the sequence to one composable node through the same path
    /// as [`Sequence::build`]: a declared time budget always becomes the
    /// node's deadline wrapper, so composing a sequence into a larger
    /// tree can never silently drop its deadline or its validation.
    ///
    /// An unrepresentable budget surfaces as a precise error at the
    /// wrapper's first tick, never as silent saturation.
    #[must_use]
    pub fn into_node(mut self) -> Node<R> {
        let budget = self.budget;
        let plain = Node(node::NodeKind::Sequence {
            children: std::mem::take(&mut self.stages),
            active: 0,
        });
        match budget {
            Some(budget) => plain.within(budget),
            None => plain,
        }
    }

    /// Validates the tree's aggregate bounds and builds it.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree exceeds the node or depth bounds.
    pub fn build(self) -> crate::Result<Tree<R>> {
        Tree::from_node(self.into_node())
    }
}

impl<R: crate::runtime::EndpointView + 'static> Default for Sequence<R> {
    fn default() -> Self {
        Self::new()
    }
}

/// One call stage awaiting its expected-response predicate or its typed
/// continuation.
pub struct CallStage<R: crate::runtime::EndpointView + ?Sized + 'static, Request, Response> {
    stages: Vec<Node<R>>,
    budget: Option<Duration>,
    call: Option<Call<Request, Response>>,
}

impl<R, Request, Response> CallStage<R, Request, Response>
where
    R: crate::runtime::EndpointView + 'static,
    Request: Message + Send + 'static,
    Response: CallResponse + 'static,
{
    /// Completes the call stage with its typed response predicate: the
    /// stage succeeds once an accepted response satisfies it.
    #[must_use]
    pub fn expect_response<F>(mut self, expect: F) -> Sequence<R>
    where
        F: Fn(&Response) -> bool + Send + 'static,
    {
        let Some(call) = self.call.take() else {
            unreachable!("the call value exists until the stage is completed");
        };
        self.stages.push(call_node(call, Box::new(expect)));
        Sequence {
            stages: self.stages,
            budget: self.budget,
        }
    }

    /// Completes the call stage with one typed continuation: the factory
    /// consumes exactly one decoded response and produces the next owned
    /// node. It never runs on refusal, uncertainty, expiry, or a decode
    /// failure, and the produced child is validated against the tree's
    /// remaining aggregate bounds before it can tick.
    #[must_use]
    pub fn then<F>(mut self, factory: F) -> Sequence<R>
    where
        F: FnOnce(Response) -> crate::Result<Node<R>> + Send + 'static,
    {
        let Some(call) = self.call.take() else {
            unreachable!("the call value exists until the stage is completed");
        };
        self.stages.push(continuation_node(call, Box::new(factory)));
        Sequence {
            stages: self.stages,
            budget: self.budget,
        }
    }
}
