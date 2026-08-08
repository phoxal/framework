//! Local launch validation and the supervised bus-open boundary.
//!
//! Everything in this module that can be decided without transport is kept
//! ahead of `BusOwner::open`: bundle selection, binary identity, config
//! deserialization, clock origin, and scheduler-input validation. The live
//! scheduler is intentionally built by the lifecycle after the potentially
//! slow bus connection succeeds.

use std::future::Future;
use std::time::Duration;

use crate::participant::api::Participant;
use crate::participant::clock::real::RealClock;
use crate::participant::clock::{ClockReading, ClockSource, TimeUnsynchronized};
use crate::participant::config::ParticipantConfig;
use crate::participant::launch::SupervisedLaunch;
use crate::participant::scheduler::AnyStepScheduler;
use phoxal_bundle::ParticipantRuntimeInputs;
use phoxal_bus::{BusConfig, BusHandle, BusOwner};
use phoxal_runtime_contract::identity::ParticipantId;
use phoxal_runtime_contract::launch::ClockMode;

use super::ShutdownController;
use super::executable::verify_current_executable;
use super::inputs::{participant_config, participant_inputs_for_launch};
use super::lifecycle::{self, BusLease};

/// All validated inputs that the lifecycle needs after the bus exists.
///
/// The supervised constructor always fills `session` with an owned
/// [`BusOwner`]. The explicit test harness uses the `Borrowed` variant, which
/// keeps caller-owned bus lifetime separate from the process launch path.
pub(crate) struct PreparedRun<R: Participant, C: ClockSource> {
    pub(crate) bus: BusHandle,
    pub(crate) session: BusLease,
    pub(crate) participant_id: ParticipantId,
    pub(crate) shutdown_grace: Duration,
    pub(crate) bundle: Option<ParticipantRuntimeInputs>,
    pub(crate) config: R::Config,
    pub(crate) clock_mode: ClockMode,
    pub(crate) clock: C,
    pub(crate) query_reply_delay: Option<Duration>,
}

/// Run a participant from the strict supervised process contract.
pub(crate) async fn run_supervised<R, S>(launch: SupervisedLaunch, shutdown: S) -> crate::Result<()>
where
    R: Participant,
    S: Future<Output = ()>,
{
    let mut shutdown = ShutdownController::new(shutdown);
    let shutdown_grace = launch.shutdown_grace;
    let (bundle, config, clock, clock_mode) = validate_launch::<R>(&launch)?;

    // One line, not a per-attempt one: a participant racing a router that has
    // not opened its listener yet can take several seconds to connect. Without
    // this, that gap looks like a silent hang rather than expected startup.
    tracing::info!(
        target: "phoxal.runtime",
        endpoints = ?launch.connect_endpoints,
        "connecting to the bus"
    );
    let (owner, bus) = tokio::select! {
        biased;
        _ = shutdown.wait() => return Ok(()),
        result = BusOwner::open(BusConfig::for_participant(
            launch.execution_id,
            launch.participant_id.clone(),
            launch.connect_endpoints.clone(),
        )) => result?,
    };

    // Do not construct the live scheduler until this connection boundary has
    // completed. The preflight above validates its inputs without creating a
    // scheduler that could outlive a failed or cancelled bus open.
    lifecycle::run(
        PreparedRun::<R, RealClock> {
            bus,
            session: BusLease::Owned(owner),
            participant_id: launch.participant_id,
            shutdown_grace,
            bundle: Some(bundle),
            config,
            clock_mode,
            clock,
            query_reply_delay: None,
        },
        &mut shutdown,
    )
    .await
}

/// Validate and select the persisted participant before opening the bus.
pub(crate) fn validate_launch<R>(
    launch: &SupervisedLaunch,
) -> crate::Result<(ParticipantRuntimeInputs, R::Config, RealClock, ClockMode)>
where
    R: Participant,
{
    // Bundle validation and exact participant selection happen before any bus
    // session exists. A malformed bundle therefore has no producer or wire
    // side effects to clean up.
    let bundle = participant_inputs_for_launch(&launch.bundle_root, &launch.participant_id)?;
    if launch.participant_id.as_str() != R::ID {
        anyhow::bail!(
            "supervised participant id '{}' does not match this binary's compiled id '{}'",
            launch.participant_id,
            R::ID
        );
    }
    if bundle.participant.kind != R::KIND {
        anyhow::bail!(
            "runtime participant '{}' has kind {:?}, but this binary declares {:?}",
            launch.participant_id,
            bundle.participant.kind,
            R::KIND
        );
    }
    verify_current_executable(bundle.participant.binary.digest)?;
    let process_config_schema: serde_json::Value = serde_json::from_str(R::Config::SCHEMA_JSON)
        .map_err(|error| {
            anyhow::anyhow!(
                "binary '{}' carries an invalid compiled config schema: {error}",
                R::ID
            )
        })?;
    if process_config_schema != bundle.participant.binary.compatibility.config_schema {
        anyhow::bail!(
            "runtime config schema for participant '{}' does not match this binary",
            launch.participant_id
        );
    }
    // Deserialize the selected config while the process is still local. A
    // custom `Deserialize` implementation may reject a value that its JSON
    // Schema accepts; that must not become a transport-visible startup error.
    let config = participant_config::<R::Config>(bundle.participant.config.as_ref())?;
    let origin = launch
        .execution_origin
        .ok_or(TimeUnsynchronized::MissingOrigin)?;
    let clock = RealClock::new(origin)?;
    let clock_mode = bundle.participant.clock;
    validate_clock_inputs::<R, _>(clock_mode, &clock)?;
    Ok((bundle, config, clock, clock_mode))
}

/// Validate scheduler selection and the initial clock discipline before any
/// supervised transport is opened. The lifecycle repeats construction after
/// the bus connects so it retains the live scheduler handle.
pub(crate) fn validate_clock_inputs<R, C>(clock_mode: ClockMode, clock: &C) -> crate::Result<()>
where
    R: Participant,
    C: ClockSource,
{
    let reading = clock.read();
    if clock_mode == ClockMode::Real
        && let ClockReading::Unsynchronized(reason) = reading
    {
        return Err(lifecycle::ClockDisciplineLost { reason }.into());
    }
    AnyStepScheduler::for_clock_mode(clock_mode, R::__step_schedule(), reading.instant())?;
    Ok(())
}
