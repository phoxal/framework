//! Typed event-capture nodes.
//!
//! `observe` activates one bounded capture of an already-admitted queued
//! input at its first tick — which is necessarily after this invocation's
//! dispatch copy, so the current cut's items are excluded exactly as
//! required. `wait_event` polls the capture for a matching event across
//! later invocations, retaining events that arrived before an unrelated
//! reply. Overflow and release are visible typed outcomes, never silent
//! misses.

use super::TreeStatus;
use super::outcome::{FailureKind, Step};
use crate::contracts::ProstPayload;
use crate::runtime::EndpointView;
use crate::runtime::capture::{CaptureHandle, CapturePoll, InputDescriptor};
use crate::runtime::context::Context;
use prost::Message as _;

/// One capture-activation leaf: allocates the typed handle beside its
/// activation node.
pub(super) struct ObserveLeaf<R: EndpointView + ?Sized + 'static, T> {
    handle: CaptureHandle<T>,
    activated: bool,
    runtime: std::marker::PhantomData<fn(&Context<'_, R>)>,
}

/// One bounded wait for a captured event matching the typed predicate.
pub(super) struct WaitEventLeaf<R: EndpointView + ?Sized + 'static, T> {
    handle: CaptureHandle<T>,
    predicate: Box<dyn Fn(&T) -> bool + Send>,
    runtime: std::marker::PhantomData<fn(&Context<'_, R>)>,
}

/// Builds the observe/wait pair over one typed input descriptor.
pub fn observe<R, T>(input: &InputDescriptor<T>) -> (super::Node<R>, CaptureHandle<T>)
where
    R: EndpointView + 'static,
    T: ProstPayload,
{
    let handle = crate::runtime::capture::allocate_handle(input.field);
    (
        super::Node(super::node::NodeKind::Call(Box::new(ObserveLeaf::new(
            handle.clone(),
        )))),
        handle,
    )
}

/// Builds the wait node over one capture handle and its typed predicate.
pub fn wait_event<R, T, F>(handle: CaptureHandle<T>, predicate: F) -> super::Node<R>
where
    R: EndpointView + 'static,
    T: ProstPayload,
    F: Fn(&T) -> bool + Send + 'static,
{
    super::Node(super::node::NodeKind::Call(Box::new(WaitEventLeaf::new(
        handle,
        Box::new(predicate),
    ))))
}

impl<R: EndpointView + ?Sized + 'static, T> ObserveLeaf<R, T> {
    pub(super) fn new(handle: CaptureHandle<T>) -> Self {
        Self {
            handle,
            activated: false,
            runtime: std::marker::PhantomData,
        }
    }
}

impl<R: EndpointView + ?Sized + 'static, T> super::leaf::ErasedLeaf<R> for ObserveLeaf<R, T>
where
    T: ProstPayload,
{
    fn tick(
        &mut self,
        context: &mut Context<'_, R>,
        _visits: &mut usize,
        _budget: &mut usize,
        _depth: usize,
    ) -> crate::Result<Step> {
        if !self.activated {
            let Some(captures) = context.captures() else {
                return Err(crate::anyhow!(
                    "behavior captures require the runtime adapter's capture registry"
                ));
            };
            if !captures.activate(
                self.handle.id,
                self.handle.field,
                context.invocation_index(),
            ) {
                return crate::Result::Ok(Step::Ended {
                    status: TreeStatus::Failed,
                    cause: Some(format!(
                        "the capture of `{}` exceeded the active-capture bound",
                        self.handle.field
                    )),
                    kind: FailureKind::UncertainEffect,
                });
            }
            self.activated = true;
        }
        crate::Result::Ok(Step::Succeeded)
    }

    fn retire(&mut self, context: &mut Context<'_, R>) -> usize {
        if let Some(captures) = context.captures() {
            captures.release(self.handle.id);
        }
        0
    }

    fn path(&self, output: &mut String) {
        output.push_str("observe");
    }
}

impl<R: EndpointView + ?Sized + 'static, T> WaitEventLeaf<R, T> {
    pub(super) fn new(handle: CaptureHandle<T>, predicate: Box<dyn Fn(&T) -> bool + Send>) -> Self {
        Self {
            handle,
            predicate,
            runtime: std::marker::PhantomData,
        }
    }
}

impl<R: EndpointView + ?Sized + 'static, T> super::leaf::ErasedLeaf<R> for WaitEventLeaf<R, T>
where
    T: ProstPayload,
{
    fn tick(
        &mut self,
        context: &mut Context<'_, R>,
        _visits: &mut usize,
        _budget: &mut usize,
        _depth: usize,
    ) -> crate::Result<Step> {
        let Some(captures) = context.captures() else {
            return Err(crate::anyhow!(
                "behavior captures require the runtime adapter's capture registry"
            ));
        };
        loop {
            match captures.poll::<T>(self.handle.id, |bytes| {
                let wire = <T as ProstPayload>::Wire::decode(bytes)
                    .map_err(|error| crate::anyhow!("{error}"))?;
                T::try_from_wire(wire).map_err(|error| crate::anyhow!("{error}"))
            }) {
                CapturePoll::Pending => return crate::Result::Ok(Step::Running),
                CapturePoll::Gone => {
                    return crate::Result::Ok(Step::Ended {
                        status: TreeStatus::Failed,
                        cause: Some("the capture was released before its event arrived".to_owned()),
                        kind: FailureKind::UncertainEffect,
                    });
                }
                CapturePoll::Overflow => {
                    return crate::Result::Ok(Step::Ended {
                        status: TreeStatus::Failed,
                        cause: Some(
                            "the capture overflowed its bounds and dropped events".to_owned(),
                        ),
                        kind: FailureKind::UncertainEffect,
                    });
                }
                // A retained item that fails its typed decode is an
                // integrity failure of this execution, distinct from
                // bounded loss: fault the invocation.
                CapturePoll::Corrupt(detail) => {
                    return Err(crate::anyhow!(
                        "captured event of `{}` failed integrity: {detail}",
                        self.handle.field
                    ));
                }
                CapturePoll::Event(event) => {
                    if (self.predicate)(&event) {
                        captures.release(self.handle.id);
                        return crate::Result::Ok(Step::Succeeded);
                    }
                    // A non-matching event is consumed: it was retained
                    // for this wait and does not replay into a later one.
                }
            }
        }
    }

    fn retire(&mut self, context: &mut Context<'_, R>) -> usize {
        if let Some(captures) = context.captures() {
            captures.release(self.handle.id);
        }
        0
    }

    fn path(&self, output: &mut String) {
        output.push_str("wait_event");
    }
}
