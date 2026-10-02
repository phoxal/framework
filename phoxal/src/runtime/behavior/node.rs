//! The private node representation and its bounded traversal.
//!
//! Composites remember their active children so successful effects never
//! replay; halts retire owned subtrees exactly once. Every tick is
//! visit-bounded, carries the subtree's depth from the root, and every
//! dynamic child is admitted through one bounded structural validator —
//! nodes and aggregate depth — before it can tick.

use super::TreeStatus;
use super::leaf::{
    ActionHalt, ActionLeaf, ActionTick, CallLeaf, ContinuationLeaf, ErasedLeaf, Predicate,
};
use super::outcome::{FailureKind, Step};
use crate::contracts::Call;
use crate::runtime::EndpointView;
use crate::runtime::context::Context;
use crate::runtime::core::ExecutionTime;

/// One node in a behavior tree, with its remembered progress. The
/// composition surface is the [`super::Node`] wrapper and its
/// constructors; this representation stays private so nesting stays
/// structurally bounded by validated construction.
pub(in crate::runtime::behavior) enum NodeKind<R: EndpointView + ?Sized + 'static> {
    /// Succeeds or stays running from one frozen predicate.
    WaitUntil(Predicate<R>),
    /// Succeeds or fails immediately from one frozen predicate.
    Condition(Predicate<R>),
    /// One typed call with its response predicate.
    Call(Box<dyn ErasedLeaf<R> + Send>),
    /// Waits from its own first activation for the given duration.
    Delay {
        duration: std::time::Duration,
        deadline: Option<ExecutionTime>,
    },
    /// Bounds its decorated subtree from the subtree's first admitted
    /// tick; timeout wins before further child work at the deadline.
    Within {
        budget: std::time::Duration,
        deadline: Option<ExecutionTime>,
        child: Option<Box<super::Node<R>>>,
    },
    /// Remembers the running child; advances after success.
    Sequence {
        children: Vec<super::Node<R>>,
        active: usize,
    },
    /// Remembers the selected running child; tries the next after an
    /// eligible behavior failure.
    Selector {
        children: Vec<super::Node<R>>,
        active: usize,
    },
    /// Rechecks its predicate before each child tick and halts the child
    /// exactly once when the predicate stops holding.
    Guard {
        predicate: Predicate<R>,
        child: Option<Box<super::Node<R>>>,
        halted: bool,
    },
    /// Ticks children cooperatively in declaration order; the first
    /// failing child halts the remaining active children.
    All {
        children: Vec<super::Node<R>>,
        done: Vec<bool>,
    },
    /// Ticks children in declaration order; the first terminal child wins
    /// and the losers are halted locally.
    Race {
        children: Vec<super::Node<R>>,
        done: Vec<bool>,
    },
    /// Finite explicit repetition: a fresh child only after the previous
    /// one succeeds, started no earlier than the invocation after the
    /// success. `resume_at` records that invocation's index; two ticks of
    /// the same invocation never run two attempts.
    Repeat {
        count: u32,
        attempt: u32,
        factory: Box<dyn Fn(u32) -> crate::Result<super::Node<R>> + Send>,
        child: Option<Box<super::Node<R>>>,
        resume_at: Option<u64>,
        /// The exact node reservation charged when the current child was
        /// admitted. It returns whole when the child's scope is released,
        /// however the live graph has shrunk since admission: static
        /// descendants removed by destructive retirement keep their
        /// reservation until this dynamic scope ends.
        charged: usize,
    },
    /// One custom action with its optional local halt hook.
    Action(ActionLeaf<R>),
}

/// The public composition node. Built through the constructors in
/// [`super`], [`super::Sequence::into_node`], and
/// [`super::CallStage::then`]; lowered to the same engine as the fluent
/// sequence.
pub struct Node<R: EndpointView + ?Sized + 'static>(pub(super) NodeKind<R>);

/// Builds one typed call leaf node.
pub(super) fn call_node<R, Request, Response>(
    call: Call<Request, Response>,
    expect: Box<dyn Fn(&Response) -> bool + Send>,
) -> Node<R>
where
    R: EndpointView + 'static,
    Request: prost::Message + Send + 'static,
    Response: crate::runtime::input::CallResponse + 'static,
{
    Node(NodeKind::Call(Box::new(CallLeaf::new(call, expect))))
}

/// Builds one typed continuation leaf node.
pub(super) fn continuation_node<R, Request, Response>(
    call: Call<Request, Response>,
    factory: super::leaf::ContinuationFactory<R, Response>,
) -> Node<R>
where
    R: EndpointView + 'static,
    Request: prost::Message + Send + 'static,
    Response: crate::runtime::input::CallResponse + 'static,
{
    Node(NodeKind::Call(Box::new(ContinuationLeaf::new(
        call, factory,
    ))))
}

/// Builds one custom action leaf node.
pub(super) fn action_node<R>(tick: ActionTick<R>, halt: Option<ActionHalt<R>>) -> Node<R>
where
    R: EndpointView + 'static,
{
    Node(NodeKind::Action(ActionLeaf::new(tick, halt)))
}

/// Measures one subtree's node count and depth with bounded work.
///
/// The traversal visits borrowed children directly — no intermediate
/// collection — and rejects the moment either bound is exceeded, before
/// the whole caller-created graph is counted: an oversized or over-deep
/// graph costs at most `node_limit + depth_limit` visits and `depth_limit`
/// stack frames. This is the single structural validator —
/// [`super::Tree::from_node`] uses it for static construction and the
/// repeat/continuation leaves use it for dynamic admission at their
/// insertion site. Validation is framework work: a caller's factory
/// already paid to build the graph, and rejection releases it through
/// the bounded iterative drop, never a recursive unwind.
pub(super) fn measure_bounded<R>(
    node: &Node<R>,
    node_limit: usize,
    depth_limit: usize,
    what: &'static str,
) -> crate::Result<(usize, usize)>
where
    R: EndpointView + ?Sized + 'static,
{
    fn walk<R>(
        node: &Node<R>,
        node_allowance: &mut usize,
        depth_allowance: usize,
        what: &'static str,
    ) -> crate::Result<(usize, usize)>
    where
        R: EndpointView + ?Sized + 'static,
    {
        if *node_allowance == 0 {
            return Err(crate::anyhow!("{what} exceeds {} nodes", super::MAX_NODES));
        }
        *node_allowance -= 1;
        if depth_allowance == 0 {
            return Err(crate::anyhow!("{what} exceeds {} depth", super::MAX_DEPTH));
        }
        let mut nodes = 1;
        let mut depth = 1;
        let mut descend = |child: &Node<R>| -> crate::Result<()> {
            let (child_nodes, child_depth) =
                walk(child, node_allowance, depth_allowance - 1, what)?;
            nodes += child_nodes;
            depth = depth.max(1 + child_depth);
            Ok(())
        };
        match &node.0 {
            NodeKind::WaitUntil(_)
            | NodeKind::Condition(_)
            | NodeKind::Call(_)
            | NodeKind::Delay { .. }
            | NodeKind::Action(_) => {}
            NodeKind::Within { child, .. } | NodeKind::Guard { child, .. } => {
                if let Some(child) = child.as_deref() {
                    descend(child)?;
                }
            }
            NodeKind::Sequence { children, .. }
            | NodeKind::Selector { children, .. }
            | NodeKind::All { children, .. }
            | NodeKind::Race { children, .. } => {
                for child in children {
                    descend(child)?;
                }
            }
            // A repeat's dynamic child does not exist yet; its later
            // admission is validated separately at the insertion site.
            NodeKind::Repeat { child, .. } => {
                if let Some(child) = child.as_deref() {
                    descend(child)?;
                }
            }
        }
        Ok((nodes, depth))
    }
    let mut node_allowance = node_limit;
    walk(node, &mut node_allowance, depth_limit, what)
}

impl<R: EndpointView + ?Sized + 'static> Node<R> {
    /// Bounds this subtree from its first admitted tick. An
    /// unrepresentable budget surfaces as a precise error at the first
    /// tick, never as silent saturation.
    #[must_use]
    pub fn within(self, budget: std::time::Duration) -> Self {
        Self(NodeKind::Within {
            budget,
            deadline: None,
            child: Some(Box::new(self)),
        })
    }

    /// Validates the aggregate bounds and builds one owned tree.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree exceeds [`super::MAX_NODES`] nodes
    /// or [`super::MAX_DEPTH`] depth.
    pub fn build(self) -> crate::Result<super::Tree<R>>
    where
        R: Sized,
    {
        super::Tree::from_node(self)
    }

    pub(super) fn tick(
        &mut self,
        context: &mut Context<'_, R>,
        visits: &mut usize,
        budget: &mut usize,
        depth: usize,
    ) -> crate::Result<Step> {
        *visits += 1;
        if *visits > super::MAX_VISITS_PER_TICK {
            return Err(crate::anyhow!(
                "behavior tree exceeded {} node visits in one tick",
                super::MAX_VISITS_PER_TICK
            ));
        }
        self.0.tick(context, visits, budget, depth)
    }

    /// Halts this subtree, retiring every owned call ticket and dynamic
    /// child exactly once; returns the reclaimed dynamic-node allowance.
    pub(super) fn retire(&mut self, context: &mut Context<'_, R>) -> usize {
        self.0.retire(context)
    }

    /// Appends this subtree's bounded path segment to the diagnostic.
    pub(super) fn path(&self, output: &mut String) {
        self.0.path(output);
    }
}

/// Retires one optionally-owned child, returning its reclaimed dynamic
/// allowance. Ownership is taken first so the walk and the later drop
/// both see an empty slot.
fn retire_taken<R>(child: &mut Option<Box<Node<R>>>, context: &mut Context<'_, R>) -> usize
where
    R: EndpointView + ?Sized + 'static,
{
    if let Some(mut retired) = child.take() {
        retired.retire(context)
    } else {
        0
    }
}

/// Processes one child's terminal success for a repeat: releases the
/// child's scope exactly once — returning the recorded admission charge
/// plus any nested dynamic owners' own credits — latches the attempt,
/// and succeeds only when every requested attempt succeeded; the next
/// attempt starts no earlier than the next invocation.
fn repeat_child_succeeded<R>(
    child: &mut Option<Box<Node<R>>>,
    charged: &mut usize,
    budget: &mut usize,
    attempt: &mut u32,
    count: u32,
    resume_at: &mut Option<u64>,
    context: &mut Context<'_, R>,
) -> Step
where
    R: EndpointView + ?Sized + 'static,
{
    if let Some(mut retired) = child.take() {
        // The recorded charge returns whole, however the live graph has
        // shrunk since admission; `retire` credits only the separately
        // charged dynamic owners nested inside the child, never a
        // recursively recomputed size.
        *budget += *charged;
        *charged = 0;
        *budget += retired.retire(context);
    }
    *attempt += 1;
    if *attempt >= count {
        Step::Succeeded
    } else {
        // The next attempt is eligible no earlier than the invocation
        // after this success.
        *resume_at = Some(context.invocation_index().saturating_add(1));
        Step::Running
    }
}

impl<R: EndpointView + ?Sized + 'static> NodeKind<R> {
    fn tick(
        &mut self,
        context: &mut Context<'_, R>,
        visits: &mut usize,
        budget: &mut usize,
        depth: usize,
    ) -> crate::Result<Step> {
        match self {
            NodeKind::WaitUntil(predicate) => crate::Result::Ok(if predicate(context) {
                Step::Succeeded
            } else {
                Step::Running
            }),
            NodeKind::Condition(predicate) => crate::Result::Ok(if predicate(context) {
                Step::Succeeded
            } else {
                Step::Ended {
                    status: TreeStatus::Refused,
                    cause: Some("the condition predicate did not hold".to_owned()),
                    kind: FailureKind::DomainRefusal,
                }
            }),
            NodeKind::Call(leaf) => leaf.tick(context, visits, budget, depth),
            NodeKind::Action(action) => action.tick(context, visits, budget, depth),
            NodeKind::Delay { duration, deadline } => {
                let now = context.now();
                if deadline.is_none() {
                    *deadline = Some(absolute_expiry(now, *duration, "delay")?);
                }
                let Some(expiry) = *deadline else {
                    unreachable!("the deadline is set above");
                };
                crate::Result::Ok(if now.as_nanos() >= expiry.as_nanos() {
                    Step::Succeeded
                } else {
                    Step::Running
                })
            }
            NodeKind::Within {
                budget: bounded,
                deadline,
                child,
            } => {
                let now = context.now();
                if deadline.is_none() {
                    *deadline = Some(absolute_expiry(now, *bounded, "within")?);
                }
                // Inclusive deadline precedence: at the boundary instant
                // the timeout wins before any further child work, so a
                // result first visible there never satisfies it
                // retroactively.
                let Some(expiry) = *deadline else {
                    unreachable!("the deadline is set above");
                };
                if now.as_nanos() >= expiry.as_nanos() {
                    let reclaimed = retire_taken(child, context);
                    *budget += reclaimed;
                    return crate::Result::Ok(Step::Ended {
                        status: TreeStatus::TimedOut,
                        cause: Some(format!(
                            "the deadline budget of {:?} elapsed before the subtree completed",
                            bounded
                        )),
                        kind: FailureKind::UncertainEffect,
                    });
                }
                let Some(child) = child.as_mut() else {
                    return crate::Result::Ok(Step::Running);
                };
                match child.tick(context, visits, budget, depth + 1)? {
                    Step::Succeeded => crate::Result::Ok(Step::Succeeded),
                    Step::Ended {
                        status,
                        cause,
                        kind,
                    } => crate::Result::Ok(Step::Ended {
                        status,
                        cause,
                        kind,
                    }),
                    Step::Running => crate::Result::Ok(Step::Running),
                    Step::SucceededWith(_) => unreachable!("no child hands a payload upward"),
                }
            }
            NodeKind::Sequence { children, active } => {
                while *active < children.len() {
                    match children[*active].tick(context, visits, budget, depth + 1)? {
                        Step::Succeeded | Step::SucceededWith(_) => *active += 1,
                        Step::Ended {
                            status,
                            cause,
                            kind,
                        } => {
                            return crate::Result::Ok(Step::Ended {
                                status,
                                cause,
                                kind,
                            });
                        }
                        Step::Running => return crate::Result::Ok(Step::Running),
                    }
                }
                crate::Result::Ok(Step::Succeeded)
            }
            NodeKind::Selector { children, active } => {
                while *active < children.len() {
                    match children[*active].tick(context, visits, budget, depth + 1)? {
                        Step::Succeeded | Step::SucceededWith(_) => {
                            return crate::Result::Ok(Step::Succeeded);
                        }
                        Step::Running => return crate::Result::Ok(Step::Running),
                        // Only an explicitly eligible failure tries the
                        // next branch; an uncertain effect stops the tree
                        // rather than authorizing a conflicting request.
                        Step::Ended {
                            kind,
                            status,
                            cause,
                        } if kind.fallback_eligible() => {
                            // The failed branch's scope ends here: retire
                            // its remaining resources before entering the
                            // replacement, so a branch capture cannot leak
                            // across the fallback's lifetime. Retirement
                            // that abandoned an outstanding remote effect
                            // withdraws fallback eligibility: the refusal
                            // was safe, but the abandoned effect is not.
                            let cause =
                                cause.unwrap_or_else(|| "the selector branch failed".to_owned());
                            *budget += children[*active].retire(context);
                            if context.take_abandoned_effects() {
                                return crate::Result::Ok(Step::Ended {
                                    status,
                                    cause: Some(format!(
                                        "{cause}; retiring the failed branch abandoned an \
                                         unresolved remote effect"
                                    )),
                                    kind: FailureKind::UncertainEffect,
                                });
                            }
                            *active += 1;
                        }
                        Step::Ended {
                            status,
                            cause,
                            kind,
                        } => {
                            return crate::Result::Ok(Step::Ended {
                                status,
                                cause,
                                kind,
                            });
                        }
                    }
                }
                crate::Result::Ok(Step::Ended {
                    status: TreeStatus::Refused,
                    cause: Some("every selector branch failed its domain predicate".to_owned()),
                    kind: FailureKind::DomainRefusal,
                })
            }
            NodeKind::Guard {
                predicate,
                child,
                halted,
            } => {
                if *halted || !predicate(context) {
                    if !*halted {
                        // The guard's stop abandons its subtree while it
                        // may still be runnable: retire it now — the
                        // halted flag gates re-ticking, never cleanup.
                        *budget += retire_taken(child, context);
                        *halted = true;
                    }
                    // Local retirement is not proof that an abandoned
                    // remote effect stopped: an outstanding call makes
                    // this stop an uncertain effect, never a domain-safe
                    // refusal a selector could fall back from.
                    let uncertain = context.take_abandoned_effects();
                    return crate::Result::Ok(Step::Ended {
                        status: TreeStatus::Refused,
                        cause: Some("the guard predicate stopped holding".to_owned()),
                        kind: if uncertain {
                            FailureKind::UncertainEffect
                        } else {
                            FailureKind::DomainRefusal
                        },
                    });
                }
                let Some(child) = child.as_mut() else {
                    return crate::Result::Ok(Step::Succeeded);
                };
                match child.tick(context, visits, budget, depth + 1)? {
                    Step::Succeeded => crate::Result::Ok(Step::Succeeded),
                    Step::Ended {
                        status,
                        cause,
                        kind,
                    } => {
                        *halted = true;
                        crate::Result::Ok(Step::Ended {
                            status,
                            cause,
                            kind,
                        })
                    }
                    Step::Running => crate::Result::Ok(Step::Running),
                    Step::SucceededWith(_) => unreachable!("no child hands a payload upward"),
                }
            }
            NodeKind::All { children, done } => {
                let mut any_running = false;
                for (index, child) in children.iter_mut().enumerate() {
                    if done[index] {
                        continue;
                    }
                    match child.tick(context, visits, budget, depth + 1)? {
                        Step::Succeeded | Step::SucceededWith(_) => done[index] = true,
                        // The first failing child halts the remaining
                        // active children and propagates its outcome; a
                        // sibling retirement that abandoned an outstanding
                        // call upgrades the outcome to an uncertain effect.
                        Step::Ended {
                            status,
                            cause,
                            kind,
                        } => {
                            let mut kind = kind;
                            for (other, child) in children.iter_mut().enumerate() {
                                if !done[other] {
                                    *budget += child.retire(context);
                                    done[other] = true;
                                }
                            }
                            if context.take_abandoned_effects() {
                                kind = FailureKind::UncertainEffect;
                            }
                            return crate::Result::Ok(Step::Ended {
                                status,
                                cause,
                                kind,
                            });
                        }
                        Step::Running => any_running = true,
                    }
                }
                crate::Result::Ok(if any_running {
                    Step::Running
                } else {
                    Step::Succeeded
                })
            }
            NodeKind::Race { children, done } => {
                for (index, child) in children.iter_mut().enumerate() {
                    if done[index] {
                        continue;
                    }
                    match child.tick(context, visits, budget, depth + 1)? {
                        Step::Running => {}
                        // The first terminal child — success or failure —
                        // wins in declaration order; every loser is
                        // halted locally, and a loser retirement that
                        // abandoned an outstanding call upgrades a
                        // refusing outcome to an uncertain effect.
                        outcome => {
                            for (other, loser) in children.iter_mut().enumerate() {
                                if other != index && !done[other] {
                                    *budget += loser.retire(context);
                                    done[other] = true;
                                }
                            }
                            done[index] = true;
                            let uncertain = context.take_abandoned_effects();
                            return crate::Result::Ok(match outcome {
                                Step::Succeeded | Step::SucceededWith(_) => Step::Succeeded,
                                Step::Ended {
                                    status,
                                    cause,
                                    kind,
                                } => Step::Ended {
                                    status,
                                    cause,
                                    kind: if uncertain {
                                        FailureKind::UncertainEffect
                                    } else {
                                        kind
                                    },
                                },
                                Step::Running => unreachable!("matched above"),
                            });
                        }
                    }
                }
                crate::Result::Ok(Step::Running)
            }
            NodeKind::Repeat {
                count,
                attempt,
                factory,
                child,
                resume_at,
                charged,
            } => {
                // One unified path: tick the existing child, or admit one
                // fresh child for the current attempt through the bounded
                // structural validator; the child's terminal result then
                // flows through the same accounting either way.
                let step = if let Some(active) = child.as_mut() {
                    active.tick(context, visits, budget, depth + 1)?
                } else {
                    // One invocation boundary separates attempts: the
                    // authoritative invocation index gates eligibility, so
                    // two ticks of the same invocation — same context or
                    // not — never start two attempts.
                    if let Some(resume_at) = *resume_at
                        && context.invocation_index() < resume_at
                    {
                        return crate::Result::Ok(Step::Running);
                    }
                    *resume_at = None;
                    if *attempt >= *count {
                        return crate::Result::Ok(Step::Succeeded);
                    }
                    let fresh = factory(*attempt)?;
                    // Admission checks both the remaining node allowance
                    // and the aggregate depth at this insertion site
                    // before the child can ever tick. The charged amount
                    // is recorded here and returns whole exactly once at
                    // release; live size is never recomputed for credit.
                    let (nodes, _) = measure_bounded(
                        &fresh,
                        *budget,
                        super::MAX_DEPTH - depth - 1,
                        "repeat child",
                    )?;
                    *budget = budget
                        .checked_sub(nodes)
                        .ok_or_else(|| crate::anyhow!("repeat child exceeds node capacity"))?;
                    *charged = nodes;
                    let mut boxed = Box::new(fresh);
                    let step = boxed.tick(context, visits, budget, depth + 1)?;
                    *child = Some(boxed);
                    step
                };
                match step {
                    Step::Succeeded | Step::SucceededWith(_) => {
                        crate::Result::Ok(repeat_child_succeeded(
                            child, charged, budget, attempt, *count, resume_at, context,
                        ))
                    }
                    Step::Ended {
                        status,
                        cause,
                        kind,
                    } => crate::Result::Ok(Step::Ended {
                        status,
                        cause,
                        kind,
                    }),
                    Step::Running => crate::Result::Ok(Step::Running),
                }
            }
        }
    }

    /// Releases this subtree's remaining resources. Retirement is
    /// unconditional across children and idempotent: a done or halted
    /// flag records that a child will not tick again, never that its
    /// cleanup ran — leaves make their own release once-only, so a scope
    /// may walk a finished child again without double-freeing, and a
    /// successful observe's capture survives exactly until its consuming
    /// wait or the end of its containing scope.
    fn retire(&mut self, context: &mut Context<'_, R>) -> usize {
        let mut reclaimed = 0_usize;
        match self {
            NodeKind::WaitUntil(_) | NodeKind::Condition(_) | NodeKind::Delay { .. } => {}
            NodeKind::Call(leaf) => {
                reclaimed += leaf.retire(context);
            }
            NodeKind::Action(action) => {
                reclaimed += action.retire(context);
            }
            NodeKind::Within { child, .. } => {
                reclaimed += retire_taken(child, context);
            }
            NodeKind::Sequence { children, .. } | NodeKind::Selector { children, .. } => {
                for child in children {
                    reclaimed += child.retire(context);
                }
            }
            NodeKind::Guard { child, .. } => {
                reclaimed += retire_taken(child, context);
            }
            NodeKind::All { children, .. } | NodeKind::Race { children, .. } => {
                for child in children {
                    reclaimed += child.retire(context);
                }
            }
            NodeKind::Repeat { child, charged, .. } => {
                if let Some(mut retired) = child.take() {
                    // The recorded admission charge returns whole; nested
                    // dynamic owners inside the child credit their own
                    // separate charges through the walk.
                    reclaimed += *charged;
                    *charged = 0;
                    reclaimed += retired.retire(context);
                }
            }
        }
        reclaimed
    }

    fn path(&self, output: &mut String) {
        match self {
            NodeKind::WaitUntil(_) => output.push_str("wait"),
            NodeKind::Condition(_) => output.push_str("condition"),
            NodeKind::Call(leaf) => leaf.path(output),
            NodeKind::Delay { .. } => output.push_str("delay"),
            NodeKind::Action(_) => output.push_str("action"),
            NodeKind::Within { child, .. } => {
                output.push_str("within.");
                if let Some(child) = child.as_ref() {
                    child.path(output);
                } else {
                    output.push_str("(expired)");
                }
            }
            NodeKind::Sequence { children, active } => {
                output.push_str(&format!("sequence[{active}]"));
                if let Some(child) = children.get(*active) {
                    output.push('.');
                    child.path(output);
                }
            }
            NodeKind::Selector { children, active } => {
                output.push_str(&format!("selector[{active}]"));
                if let Some(child) = children.get(*active) {
                    output.push('.');
                    child.path(output);
                }
            }
            NodeKind::Guard { child, halted, .. } => {
                output.push_str("guard.");
                if *halted {
                    output.push_str("(halted)");
                } else if let Some(child) = child.as_ref() {
                    child.path(output);
                } else {
                    output.push_str("(retired)");
                }
            }
            NodeKind::All { children, done } => {
                output.push_str("all");
                Self::parallel_path("all", children, done, output);
            }
            NodeKind::Race { children, done } => {
                Self::parallel_path("race", children, done, output);
            }
            NodeKind::Repeat { attempt, child, .. } => {
                output.push_str(&format!("repeat[{attempt}]"));
                if let Some(child) = child.as_ref() {
                    output.push('.');
                    child.path(output);
                }
            }
        }
    }
}

impl<R: EndpointView + ?Sized + 'static> NodeKind<R> {
    /// Appends the shared all/race diagnostic: the active child indexes
    /// and the first active child's path.
    fn parallel_path(name: &str, children: &[super::Node<R>], done: &[bool], output: &mut String) {
        output.push_str(name);
        let active: Vec<usize> = done
            .iter()
            .enumerate()
            .filter(|(_, done)| !**done)
            .map(|(index, _)| index)
            .collect();
        output.push_str(&format!("[{:?}]", active));
        if let Some(first) = active.first()
            && let Some(child) = children.get(*first)
        {
            output.push('.');
            child.path(output);
        }
    }
}

impl<R: EndpointView + ?Sized + 'static> Drop for Node<R> {
    fn drop(&mut self) {
        // A caller-created graph can be deeper than the engine ever
        // traversed — validation rejects at the depth bound without
        // visiting the rest — so the default recursive drop could unwind
        // an unbounded caller stack. The worklist bounds teardown: each
        // node's children are taken out before the node itself drops,
        // and leaf drops are their own once-only cleanup.
        let mut worklist: Vec<Node<R>> = Vec::new();
        take_children(&mut self.0, &mut worklist);
        while let Some(mut node) = worklist.pop() {
            take_children(&mut node.0, &mut worklist);
        }
    }
}

/// Moves one node kind's owned children onto the teardown worklist
/// without dropping anything recursively.
fn take_children<R>(kind: &mut NodeKind<R>, worklist: &mut Vec<Node<R>>)
where
    R: EndpointView + ?Sized + 'static,
{
    match kind {
        NodeKind::WaitUntil(_)
        | NodeKind::Condition(_)
        | NodeKind::Call(_)
        | NodeKind::Delay { .. }
        | NodeKind::Action(_) => {}
        NodeKind::Within { child, .. }
        | NodeKind::Guard { child, .. }
        | NodeKind::Repeat { child, .. } => {
            if let Some(node) = child.take() {
                worklist.push(*node);
            }
        }
        NodeKind::Sequence { .. }
        | NodeKind::Selector { .. }
        | NodeKind::All { .. }
        | NodeKind::Race { .. } => {
            let drained = std::mem::take(match kind {
                NodeKind::Sequence { children, .. }
                | NodeKind::Selector { children, .. }
                | NodeKind::All { children, .. }
                | NodeKind::Race { children, .. } => children,
                _ => unreachable!("matched above"),
            });
            worklist.extend(drained);
        }
    }
}

/// Computes one absolute execution-time expiry, rejecting sums the
/// execution timeline cannot represent.
fn absolute_expiry(
    now: ExecutionTime,
    duration: std::time::Duration,
    what: &str,
) -> crate::Result<ExecutionTime> {
    let expiry = u128::from(now.as_nanos()) + duration.as_nanos();
    if expiry > u64::MAX as u128 {
        return Err(crate::anyhow!(
            "the {what} budget of {duration:?} is unrepresentable at this execution time"
        ));
    }
    Ok(ExecutionTime::from_nanos(expiry as u64))
}
