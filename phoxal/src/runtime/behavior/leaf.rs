//! Erased callable, continuation, and custom-action leaves.
//!
//! Leaves are the only nodes that touch calls, responses, and application
//! closures; every composite node merely orders and halts them. A leaf
//! submits at most once, polls its typed completion by exclusive ticket,
//! and retires its local ownership exactly once when halted.

use super::TreeStatus;
use super::node::Node;
use super::outcome::{FailureKind, Step, outcome_of};
use crate::contracts::Call;
use crate::runtime::EndpointView;
use crate::runtime::context::Context;
use crate::runtime::input::{CallResponse, RequestError};
use crate::runtime::outputs::CallTicket;
use prost::Message;
use std::marker::PhantomData;

/// A type-erased leaf: ticks against the frozen cut and retires local
/// ownership without another tick.
pub(in crate::runtime::behavior) trait ErasedLeaf<R: EndpointView + ?Sized>:
    Send
{
    fn tick(
        &mut self,
        context: &mut Context<'_, R>,
        visits: &mut usize,
        budget: &mut usize,
        depth: usize,
    ) -> crate::Result<Step>;
    /// Retires local ownership, returning the reclaimed dynamic-node
    /// allowance of retired subtrees.
    fn retire(&mut self, context: &mut Context<'_, R>) -> usize;
    /// Appends this leaf's bounded diagnostic segment. A typed
    /// continuation exposes its active owned child so the live stage is
    /// distinguishable from the awaiting stage.
    fn path(&self, output: &mut String) {
        output.push_str("call");
    }
}

/// One owned frozen-cut predicate.
pub type Predicate<R> = Box<dyn Fn(&Context<'_, R>) -> bool + Send>;

/// One owned continuation factory: consumes one decoded response and
/// produces the next owned node.
pub type ContinuationFactory<R, Response> =
    Box<dyn FnOnce(Response) -> crate::Result<Node<R>> + Send>;

/// One owned response predicate.
type ResponsePredicate<Response> = Box<dyn Fn(&Response) -> bool + Send>;

/// One typed call leaf carrying its inert call, response predicate, and
/// the ticket of its single tree-owned submission.
pub(in crate::runtime::behavior) struct CallLeaf<
    R: EndpointView + ?Sized + 'static,
    Request,
    Response,
> {
    pub(super) call: Option<Call<Request, Response>>,
    pub(super) expect: ResponsePredicate<Response>,
    ticket: Option<CallTicket<Response>>,
    /// Whether this leaf consumed its completion: an unconsumed ticket at
    /// retirement is evidence of an abandoned, unresolved remote effect.
    consumed: bool,
    runtime: PhantomData<fn(&Context<'_, R>)>,
}

impl<R, Request, Response> CallLeaf<R, Request, Response>
where
    R: EndpointView + ?Sized + 'static,
{
    pub(in crate::runtime::behavior) fn new(
        call: Call<Request, Response>,
        expect: ResponsePredicate<Response>,
    ) -> Self {
        Self {
            call: Some(call),
            expect,
            ticket: None,
            consumed: false,
            runtime: PhantomData,
        }
    }
}

impl<R, Request, Response> ErasedLeaf<R> for CallLeaf<R, Request, Response>
where
    R: EndpointView + ?Sized + 'static,
    Request: Message + Send + 'static,
    Response: CallResponse + 'static,
{
    fn tick(
        &mut self,
        context: &mut Context<'_, R>,
        _visits: &mut usize,
        _budget: &mut usize,
        _depth: usize,
    ) -> crate::Result<Step> {
        if self.ticket.is_none() {
            let call = match self.call.take() {
                Some(call) => call,
                None => unreachable!("the call value exists until the first tick"),
            };
            // Tree-owned staging: the leaf keeps the ticket, so no direct
            // completion handler ever receives this call's reply.
            self.ticket = Some(context.send(call)?);
            return crate::Result::Ok(Step::Running);
        }
        let Some(ticket) = self.ticket.as_ref() else {
            unreachable!("the ticket is set above");
        };
        match completion_step(ticket, context)? {
            Step::SucceededWith(response) => {
                self.consumed = true;
                crate::Result::Ok(if (self.expect)(&response) {
                    Step::Succeeded
                } else {
                    Step::Ended {
                        status: TreeStatus::Refused,
                        cause: Some(
                            "an accepted response did not satisfy the expected-response predicate"
                                .to_owned(),
                        ),
                        kind: FailureKind::DomainRefusal,
                    }
                })
            }
            Step::Running => crate::Result::Ok(Step::Running),
            Step::Ended {
                status,
                cause,
                kind,
            } => {
                self.consumed = true;
                crate::Result::Ok(Step::Ended {
                    status,
                    cause,
                    kind,
                })
            }
            Step::Succeeded => unreachable!("completion_step hands a payload or ends"),
        }
    }

    fn retire(&mut self, context: &mut Context<'_, R>) -> usize {
        if let Some(ticket) = self.ticket.take() {
            // An unconsumed ticket whose remote effect may have executed
            // is abandoned by this local retirement: record the evidence
            // so the halting composite reports uncertainty.
            if !self.consumed && context.call_may_have_executed(&ticket) {
                context.note_abandoned_effect();
            }
            context.retire_completion(&ticket);
        }
        0
    }

    fn path(&self, output: &mut String) {
        output.push_str("call");
    }
}

/// One typed continuation leaf: consumes exactly one decoded response and
/// builds one owned child from the application factory. The factory runs
/// only on a successfully decoded response; refusal, uncertainty, and
/// expiry end the continuation without activating it.
pub(in crate::runtime::behavior) struct ContinuationLeaf<
    R: EndpointView + ?Sized + 'static,
    Request,
    Response,
> {
    call: Option<Call<Request, Response>>,
    ticket: Option<CallTicket<Response>>,
    /// Whether the leaf consumed its completion: an unconsumed ticket at
    /// retirement is evidence of an abandoned, unresolved remote effect.
    consumed: bool,
    factory: Option<ContinuationFactory<R, Response>>,
    child: Option<Box<Node<R>>>,
    /// The exact node reservation charged when the built child was
    /// admitted. It returns whole when the child's scope is released,
    /// however the live graph has shrunk since admission.
    charged: usize,
    runtime: PhantomData<fn(&Context<'_, R>)>,
}

impl<R, Request, Response> ContinuationLeaf<R, Request, Response>
where
    R: EndpointView + ?Sized + 'static,
    Response: CallResponse + 'static,
{
    pub(in crate::runtime::behavior) fn new(
        call: Call<Request, Response>,
        factory: Box<dyn FnOnce(Response) -> crate::Result<Node<R>> + Send>,
    ) -> Self {
        Self {
            call: Some(call),
            ticket: None,
            consumed: false,
            factory: Some(factory),
            child: None,
            charged: 0,
            runtime: PhantomData,
        }
    }
}

impl<R, Request, Response> ErasedLeaf<R> for ContinuationLeaf<R, Request, Response>
where
    R: EndpointView + ?Sized + 'static,
    Request: Message + Send + 'static,
    Response: CallResponse + 'static,
{
    fn tick(
        &mut self,
        context: &mut Context<'_, R>,
        visits: &mut usize,
        budget: &mut usize,
        depth: usize,
    ) -> crate::Result<Step> {
        if self.ticket.is_none() {
            let call = match self.call.take() {
                Some(call) => call,
                None => unreachable!("the call value exists until the first tick"),
            };
            self.ticket = Some(context.send(call)?);
            return crate::Result::Ok(Step::Running);
        }
        if let Some(child) = self.child.as_mut() {
            return child.tick(context, visits, budget, depth + 1);
        }
        let Some(ticket) = self.ticket.as_ref() else {
            unreachable!("the ticket is set above");
        };
        match completion_step(ticket, context)? {
            Step::Running => crate::Result::Ok(Step::Running),
            Step::Ended {
                status,
                cause,
                kind,
            } => {
                self.consumed = true;
                crate::Result::Ok(Step::Ended {
                    status,
                    cause,
                    kind,
                })
            }
            Step::SucceededWith(response) => {
                self.consumed = true;
                let Some(factory) = self.factory.take() else {
                    unreachable!("the factory exists until its single activation");
                };
                // The produced child is admitted through the one bounded
                // structural validator — remaining node allowance and
                // aggregate depth at this insertion site — before it can
                // ever tick. The charged amount is recorded here and
                // returns whole exactly once at release; live size is
                // never recomputed for credit.
                let child = factory(response)?;
                let (nodes, _) = super::node::measure_bounded(
                    &child,
                    *budget,
                    super::MAX_DEPTH - depth - 1,
                    "continuation child",
                )?;
                *budget = budget
                    .checked_sub(nodes)
                    .ok_or_else(|| crate::anyhow!("continuation child exceeds node capacity"))?;
                self.charged = nodes;
                let mut built = Box::new(child);
                let step = built.tick(context, visits, budget, depth + 1);
                self.child = Some(built);
                step
            }
            Step::Succeeded => unreachable!("completion_step hands a payload or ends"),
        }
    }

    fn retire(&mut self, context: &mut Context<'_, R>) -> usize {
        let mut reclaimed = 0_usize;
        if let Some(mut child) = self.child.take() {
            // The recorded admission charge returns whole; nested dynamic
            // owners inside the child credit their own separate charges
            // through the walk.
            reclaimed += self.charged;
            self.charged = 0;
            reclaimed += child.retire(context);
        }
        if let Some(ticket) = self.ticket.take() {
            // An unconsumed ticket whose remote effect may have executed
            // is abandoned by this local retirement: record the evidence
            // so the halting composite reports uncertainty.
            if !self.consumed && context.call_may_have_executed(&ticket) {
                context.note_abandoned_effect();
            }
            context.retire_completion(&ticket);
        }
        reclaimed
    }

    fn path(&self, output: &mut String) {
        if let Some(child) = self.child.as_ref() {
            output.push_str("call.then(");
            child.path(output);
            output.push(')');
        } else {
            output.push_str("call");
        }
    }
}

/// The outcome of one custom action tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionOutcome {
    /// More bounded work remains.
    Running,
    /// The action completed successfully.
    Succeeded,
    /// The action failed its own domain predicate.
    Failed,
}

/// One custom action's incremental tick closure.
pub type ActionTick<R> = Box<dyn FnMut(&mut Context<'_, R>) -> crate::Result<ActionOutcome> + Send>;

/// One custom action's local halt hook, run exactly once when the branch
/// is retired or the action ends.
pub type ActionHalt<R> = Box<dyn FnOnce(&mut Context<'_, R>) + Send>;

/// One bounded custom action: incremental work through an ordinary
/// context tick, with an optional action-local halt hook.
pub(in crate::runtime::behavior) struct ActionLeaf<R: EndpointView + ?Sized + 'static> {
    tick: ActionTick<R>,
    halt: Option<ActionHalt<R>>,
    ended: bool,
}

impl<R: EndpointView + ?Sized + 'static> ActionLeaf<R> {
    pub(in crate::runtime::behavior) fn new(
        tick: ActionTick<R>,
        halt: Option<ActionHalt<R>>,
    ) -> Self {
        Self {
            tick,
            halt,
            ended: false,
        }
    }
}

impl<R: EndpointView + ?Sized + 'static> ErasedLeaf<R> for ActionLeaf<R> {
    fn tick(
        &mut self,
        context: &mut Context<'_, R>,
        _visits: &mut usize,
        _budget: &mut usize,
        _depth: usize,
    ) -> crate::Result<Step> {
        if self.ended {
            return crate::Result::Ok(Step::Succeeded);
        }
        match (self.tick)(context)? {
            ActionOutcome::Running => crate::Result::Ok(Step::Running),
            ActionOutcome::Succeeded => {
                if let Some(halt) = self.halt.take() {
                    halt(context);
                }
                self.ended = true;
                crate::Result::Ok(Step::Succeeded)
            }
            ActionOutcome::Failed => {
                if let Some(halt) = self.halt.take() {
                    halt(context);
                }
                self.ended = true;
                crate::Result::Ok(Step::Ended {
                    status: TreeStatus::Refused,
                    cause: Some("the custom action failed its domain predicate".to_owned()),
                    kind: FailureKind::DomainRefusal,
                })
            }
        }
    }

    fn retire(&mut self, context: &mut Context<'_, R>) -> usize {
        if let Some(halt) = self.halt.take() {
            halt(context);
        }
        self.ended = true;
        0
    }

    fn path(&self, output: &mut String) {
        output.push_str("action");
    }
}

/// Polls one ticket's completion once, handing one decoded response to
/// the caller as a payload. Integrity failures fault the invocation;
/// everything else keeps the typed classification.
fn completion_step<Response, R>(
    ticket: &CallTicket<Response>,
    context: &mut Context<'_, R>,
) -> crate::Result<Step<Response>>
where
    R: EndpointView + ?Sized + 'static,
    Response: CallResponse + 'static,
{
    let Some(completion) = context.take_completion(ticket) else {
        return crate::Result::Ok(Step::Running);
    };
    match completion.into_result() {
        Ok(response) => crate::Result::Ok(Step::SucceededWith(response)),
        Err(RequestError::Integrity(detail)) => {
            // An undecodable response is an integrity failure of this
            // execution, never a domain outcome: fault the invocation
            // instead of ending the tree or discarding the result.
            Err(crate::anyhow!(
                "tree-owned call response failed integrity: {detail}"
            ))
        }
        Err(error) => crate::Result::Ok(match outcome_of(error) {
            Step::Ended {
                status,
                cause,
                kind,
            } => Step::Ended {
                status,
                cause,
                kind,
            },
            _ => unreachable!("outcome_of always ends"),
        }),
    }
}
