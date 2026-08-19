//! The participant runner entrypoints and lifecycle orchestration.
//!
//! The implementation is split by ownership boundary: [`startup`] performs
//! all local launch validation before opening the bus, [`lifecycle`] owns
//! setup/Ready/teardown resources, and [`event_loop`] owns the serialized
//! scheduler loop. Query ingress/reply transport, process signals, teardown,
//! and bundle inputs each stay in their focused modules.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::participant::api::Participant;
use crate::participant::bus_log;
use crate::participant::clock::ClockMode;
use crate::participant::clock::ClockSource;
use crate::participant::clock::real::RealClock;
use crate::participant::launch::Launch;
use crate::participant::runner::harness::TestHarness;

pub(crate) mod event_loop;
#[allow(
    dead_code,
    reason = "compiled in every profile because a domain module never asks which profile it is in; its only consumer is a module one profile declares"
)]
pub(crate) mod harness;
pub(crate) mod inputs;
pub(crate) mod lifecycle;
pub(crate) mod query;
pub(crate) mod signal;
pub(crate) mod startup;
pub(crate) mod teardown;

#[cfg(test)]
mod tests;

use lifecycle::BusLease;
use signal::shutdown_signal;
use startup::PreparedRun;

/// A sticky lifecycle stop request. The source future is polled during every
/// asynchronous startup boundary and the request remains observable after the
/// source has completed, so a signal cannot be lost between setup and Ready.
pub(crate) struct ShutdownRequest {
    requested: AtomicBool,
    notify: tokio::sync::Notify,
}

impl ShutdownRequest {
    fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    fn trigger(&self) {
        if !self.requested.swap(true, Ordering::Release) {
            self.notify.notify_waiters();
        }
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    async fn wait(&self) {
        if self.is_requested() {
            return;
        }
        let notified = self.notify.notified();
        if self.is_requested() {
            return;
        }
        notified.await;
    }
}

/// Couples an injected shutdown source (or the Unix signal future) to a sticky
/// request that startup and the main loop can race independently.
pub(crate) struct ShutdownController<S> {
    request: Arc<ShutdownRequest>,
    source: Pin<Box<S>>,
}

impl<S> ShutdownController<S>
where
    S: Future<Output = ()>,
{
    pub(crate) fn new(source: S) -> Self {
        Self {
            request: Arc::new(ShutdownRequest::new()),
            source: Box::pin(source),
        }
    }

    pub(crate) fn is_requested(&self) -> bool {
        self.request.is_requested()
    }

    pub(crate) async fn wait(&mut self) {
        if self.request.is_requested() {
            return;
        }
        tokio::select! {
            biased;
            _ = self.request.wait() => {},
            _ = &mut self.source => self.request.trigger(),
        }
    }
}

/// Run a participant to completion on a framework-owned blocking Tokio runtime.
pub fn run<R: Participant>() -> crate::Result<()> {
    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    tokio_runtime.block_on(run_async::<R>())
}

/// Async host runner for custom Tokio mains
/// (`phoxal::tokio::run::<Participant>().await`).
pub async fn run_async<R: Participant>() -> crate::Result<()> {
    // Every real entry path passes through here first, which keeps the
    // participant's embedded metadata static from being garbage-collected by
    // the ELF linker.
    R::__retain_embedded_metadata();

    bus_log::init_tracing();
    // Parse the launch contract before opening anything. The host's signal is
    // installed immediately after parsing so startup teardown still follows the
    // same steady-state path.
    let launch = Launch::parse()?;
    let shutdown = shutdown_signal()?;

    startup::run_supervised::<R, _>(launch, shutdown).await
}

/// Run a participant on a caller-owned bus using explicit test-harness input.
/// The harness never opens or closes the bus and cannot claim a Ready lease.
#[allow(
    dead_code,
    reason = "compiled in every profile because a domain module never asks which profile it is in; its only consumer is a module one profile declares"
)]
pub async fn run_test_harness<R, S>(
    bus: &crate::bus::BusHandle,
    harness: TestHarness,
    shutdown: S,
) -> crate::Result<()>
where
    R: Participant,
    S: Future<Output = ()>,
{
    bus_log::init_tracing();
    let query_reply_delay = harness.query_reply_delay;
    let clock = RealClock::new(harness.timeline);
    let config = inputs::deserialize_config::<R::Config>(harness.config.as_ref())?;
    startup::validate_clock_inputs::<R, _>(ClockMode::Real, Some(&clock))?;
    let mut shutdown = ShutdownController::new(shutdown);
    lifecycle::run(
        PreparedRun::<R, RealClock> {
            bus: bus.clone(),
            session: BusLease::Borrowed,
            participant_id: harness.participant_id,
            shutdown_grace: harness.shutdown_grace,
            bundle: None,
            config,
            clock_mode: ClockMode::Real,
            clock: Some(clock),
            query_reply_delay,
        },
        &mut shutdown,
    )
    .await
}

/// Deterministic clock-injection seam for checked participants.
#[doc(hidden)]
#[allow(
    dead_code,
    reason = "compiled in every profile because a domain module never asks which profile it is in; its only consumer is a module one profile declares"
)]
pub async fn run_test_harness_with_clock<R, C, S>(
    bus: &crate::bus::BusHandle,
    harness: TestHarness,
    clock: C,
    shutdown: S,
) -> crate::Result<()>
where
    R: Participant + crate::__private::surface::TypedIoSurface,
    C: ClockSource,
    S: Future<Output = ()>,
{
    bus_log::init_tracing();
    let query_reply_delay = harness.query_reply_delay;
    let config = inputs::deserialize_config::<R::Config>(harness.config.as_ref())?;
    startup::validate_clock_inputs::<R, _>(ClockMode::Real, Some(&clock))?;
    let mut shutdown = ShutdownController::new(shutdown);
    lifecycle::run(
        PreparedRun::<R, C> {
            bus: bus.clone(),
            session: BusLease::Borrowed,
            participant_id: harness.participant_id,
            shutdown_grace: harness.shutdown_grace,
            bundle: None,
            config,
            clock_mode: ClockMode::Real,
            clock: Some(clock),
            query_reply_delay,
        },
        &mut shutdown,
    )
    .await
}
