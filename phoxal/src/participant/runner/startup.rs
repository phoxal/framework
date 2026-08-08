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
use phoxal_bundle::ParticipantClock;
use phoxal_bundle::ParticipantRuntimeInputs;
use phoxal_bus::{BusConfig, BusHandle, BusOwner};
use phoxal_runtime_contract::identity::{ParticipantArtifactId, ParticipantId};
use phoxal_runtime_contract::metadata::{ParticipantContract, ParticipantSchemas};
use phoxal_runtime_contract::origin::ExecutionOrigin;

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
    pub(crate) clock_mode: ParticipantClock,
    pub(crate) clock: Option<C>,
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
) -> crate::Result<(
    ParticipantRuntimeInputs,
    R::Config,
    Option<RealClock>,
    ParticipantClock,
)>
where
    R: Participant,
{
    // Bundle validation and exact participant selection happen before any bus
    // session exists. A malformed bundle therefore has no producer or wire
    // side effects to clean up.
    let bundle = participant_inputs_for_launch(&launch.bundle_root, &launch.participant_id)?;
    let compiled_artifact_id = ParticipantArtifactId::new(R::ID).map_err(|error| {
        anyhow::anyhow!(
            "binary '{}' carries an invalid compiled artifact id: {error}",
            R::ID
        )
    })?;
    if bundle.artifact.contract.id != compiled_artifact_id {
        anyhow::bail!(
            "selected artifact '{}' does not match this binary's compiled artifact id '{}'",
            bundle.artifact.contract.id,
            R::ID
        );
    }
    if bundle.artifact.contract.kind != R::KIND {
        anyhow::bail!(
            "selected artifact '{}' has kind {:?}, but this binary declares {:?}",
            bundle.artifact.contract.id,
            bundle.artifact.contract.kind,
            R::KIND
        );
    }
    let process_config_schema: serde_json::Value = serde_json::from_str(R::Config::SCHEMA_JSON)
        .map_err(|error| {
            anyhow::anyhow!(
                "binary '{}' carries an invalid compiled config schema: {error}",
                R::ID
            )
        })?;
    let expected_contract = ParticipantContract {
        id: compiled_artifact_id,
        kind: R::KIND,
        api: crate::__private::compatibility::API,
        schemas: ParticipantSchemas {
            bus: crate::__private::compatibility::BUS,
            launch: crate::__private::compatibility::LAUNCH,
            runtime: crate::__private::compatibility::RUNTIME,
        },
        requirement: R::REQUIREMENT,
        config_schema: process_config_schema,
    };
    if bundle.artifact.contract != expected_contract {
        anyhow::bail!(
            "selected artifact '{}' contract does not match this binary's compiled participant contract",
            bundle.artifact.contract.id
        );
    }
    verify_current_executable(bundle.artifact.digest)?;
    // Deserialize the selected config while the process is still local. A
    // custom `Deserialize` implementation may reject a value that its JSON
    // Schema accepts; that must not become a transport-visible startup error.
    let clock_mode = bundle.participant.clock;
    let config = participant_config::<R::Config>(bundle.participant.config.as_ref())?;
    let clock = clock_for_mode(clock_mode, launch.execution_origin)?;
    validate_clock_inputs::<R, _>(clock_mode, clock.as_ref())?;
    Ok((bundle, config, clock, clock_mode))
}

/// Select the host clock only for real participants. Simulation and clockless
/// participants deliberately ignore any supplied execution origin: simulated
/// timestamps come from the live world clock, while clockless participants do
/// not produce robot-time steps at all.
pub(crate) fn clock_for_mode(
    clock_mode: ParticipantClock,
    execution_origin: Option<ExecutionOrigin>,
) -> crate::Result<Option<RealClock>> {
    if clock_mode == ParticipantClock::Real {
        let origin = execution_origin.ok_or(TimeUnsynchronized::MissingOrigin)?;
        Ok(Some(RealClock::new(origin)?))
    } else {
        Ok(None)
    }
}

/// Validate scheduler selection and the initial clock discipline before any
/// supervised transport is opened. The lifecycle repeats construction after
/// the bus connects so it retains the live scheduler handle.
pub(crate) fn validate_clock_inputs<R, C>(
    clock_mode: ParticipantClock,
    clock: Option<&C>,
) -> crate::Result<()>
where
    R: Participant,
    C: ClockSource,
{
    let now = if clock_mode == ParticipantClock::Real {
        let reading = clock
            .map(ClockSource::read)
            .unwrap_or(ClockReading::Unsynchronized(
                TimeUnsynchronized::MissingOrigin,
            ));
        match reading {
            ClockReading::Synchronized(_) => reading.instant(),
            ClockReading::Unsynchronized(reason) => {
                return Err(lifecycle::ClockDisciplineLost { reason }.into());
            }
        }
    } else {
        None
    };
    AnyStepScheduler::validate_clock_mode(clock_mode, R::__step_schedule(), now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_origin_is_required_only_for_real_clock_mode() {
        let origin = ExecutionOrigin::mint();

        let missing_real = clock_for_mode(ParticipantClock::Real, None)
            .expect_err("real mode without an origin must fail before bus startup");
        assert!(matches!(
            missing_real.downcast_ref::<TimeUnsynchronized>(),
            Some(TimeUnsynchronized::MissingOrigin)
        ));
        assert!(
            clock_for_mode(ParticipantClock::Real, Some(origin))
                .expect("a current-host real origin is valid")
                .is_some()
        );
        assert!(
            clock_for_mode(ParticipantClock::Simulation, None)
                .expect("simulation mode does not need a host origin")
                .is_none()
        );
        assert!(
            clock_for_mode(ParticipantClock::Simulation, Some(origin))
                .expect("simulation mode ignores a supplied host origin")
                .is_none()
        );
        assert!(
            clock_for_mode(ParticipantClock::Clockless, None)
                .expect("clockless mode does not need a host origin")
                .is_none()
        );
    }
}
