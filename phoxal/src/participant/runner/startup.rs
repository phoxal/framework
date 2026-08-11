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
use phoxal_runtime_contract::metadata::ParticipantContract;
use phoxal_runtime_contract::origin::ExecutionOrigin;
use phoxal_runtime_contract::version::FrameworkVersion;

use super::ShutdownController;
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
    let recorded = bundle.artifact().contract();
    let compiled_artifact_id = ParticipantArtifactId::new(R::ID).map_err(|error| {
        anyhow::anyhow!(
            "binary '{}' carries an invalid compiled artifact id: {error}",
            R::ID
        )
    })?;
    if recorded.id != compiled_artifact_id {
        anyhow::bail!(
            "selected artifact '{}' does not match this binary's compiled artifact id '{}'",
            recorded.id,
            R::ID
        );
    }
    if recorded.kind != R::KIND {
        anyhow::bail!(
            "selected artifact '{}' has kind {:?}, but this binary declares {:?}",
            recorded.id,
            recorded.kind,
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
    // The recorded train and this binary's train have to share a compatibility
    // line, not be the same version: a participant binary from one train may
    // be launched from a bundle recorded by another train on the same line.
    // Both exact versions stay in the diagnostic, because that is the fact an
    // operator acts on.
    if !FrameworkVersion::CURRENT.is_compatible_with(recorded.framework) {
        anyhow::bail!(
            "selected artifact '{}' was built from framework {}, but this binary speaks framework \
             {} ({}); rebuild the bundle on this line",
            recorded.id,
            recorded.framework,
            FrameworkVersion::CURRENT,
            FrameworkVersion::CURRENT.compatibility_line()
        );
    }
    // Every remaining field is compared for equality, and the whole struct is
    // compared at once so a field added to the contract cannot slip past this
    // check unvalidated. `framework` is carried over from the record because
    // the line check above is its comparison.
    let expected_contract = ParticipantContract {
        framework: recorded.framework,
        id: compiled_artifact_id,
        kind: R::KIND,
        requirement: R::REQUIREMENT,
        config_schema: process_config_schema,
    };
    if recorded != &expected_contract {
        anyhow::bail!(
            "selected artifact '{}' contract does not match this binary's compiled participant contract",
            recorded.id
        );
    }
    // Deserialize the selected config while the process is still local. A
    // custom `Deserialize` implementation may reject a value that its JSON
    // Schema accepts; that must not become a transport-visible startup error.
    let clock_mode = bundle.participant().clock();
    let config = participant_config::<R::Config>(bundle.participant().config())?;
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

    use crate::prelude::*;
    use phoxal_runtime_contract::identity::ExecutionId;

    #[phoxal::brain]
    struct LineProbeBrain;

    impl Participant for LineProbeBrain {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> crate::Result<(Self::State, Self::Api)> {
            Ok(((), ()))
        }
    }

    /// Record `framework` for every artifact in a staged bundle's persisted
    /// document, standing in for a bundle produced by a different train.
    ///
    /// Only `runtime.json` is rewritten, which is what the launch path reads;
    /// the indexed assets and executables it points at are untouched, so this
    /// stages the exact situation without building a second train.
    fn record_framework(root: &std::path::Path, framework: FrameworkVersion) {
        let path = root.join(phoxal_bundle::RUNTIME_FILE);
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("the staged runtime document"))
                .expect("the staged runtime document is JSON");
        let artifacts = document
            .get_mut("artifacts")
            .and_then(serde_json::Value::as_object_mut)
            .expect("the staged document records artifacts");
        for artifact in artifacts.values_mut() {
            artifact["contract"]["framework"] = serde_json::json!(framework.to_string());
        }
        std::fs::write(
            &path,
            serde_json::to_vec(&document).expect("the rewritten document serializes"),
        )
        .expect("the rewritten document is written");
    }

    fn launch_brain(root: &std::path::Path) -> SupervisedLaunch {
        SupervisedLaunch {
            execution_id: ExecutionId::parse("10000000000000000000000000000001")
                .expect("test execution id"),
            participant_id: ParticipantId::new("brain").expect("brain participant id"),
            bundle_root: root.to_path_buf(),
            connect_endpoints: vec!["tcp/127.0.0.1:0".to_string()],
            execution_origin: Some(ExecutionOrigin::mint()),
            shutdown_grace: Duration::from_millis(
                crate::participant::launch::DEFAULT_SHUTDOWN_GRACE_MS,
            ),
        }
    }

    /// The launch validator compares compatibility lines, not versions: a
    /// binary launches from a bundle another train on its own line recorded.
    #[test]
    fn a_bundle_recorded_by_another_train_on_this_line_launches() {
        let bundle = phoxal_fixture::staged_bundle();
        let neighbour = FrameworkVersion::new(
            FrameworkVersion::CURRENT.major(),
            FrameworkVersion::CURRENT.minor(),
            FrameworkVersion::CURRENT.patch() + 1,
        );
        assert_ne!(neighbour, FrameworkVersion::CURRENT);
        assert!(FrameworkVersion::CURRENT.is_compatible_with(neighbour));
        record_framework(bundle.path(), neighbour);

        validate_launch::<LineProbeBrain>(&launch_brain(bundle.path()))
            .expect("a bundle from a neighbouring train on this line is launchable");
    }

    /// A bundle from another line is refused before any bus session exists,
    /// and the failure names both exact trains.
    #[test]
    fn a_bundle_recorded_on_another_line_is_refused_before_the_bus_opens() {
        let bundle = phoxal_fixture::staged_bundle();
        // Bumping the major crosses the line in both SemVer eras.
        let other_line = FrameworkVersion::new(FrameworkVersion::CURRENT.major() + 1, 0, 0);
        assert!(!FrameworkVersion::CURRENT.is_compatible_with(other_line));
        record_framework(bundle.path(), other_line);

        let error = validate_launch::<LineProbeBrain>(&launch_brain(bundle.path()))
            .expect_err("a bundle from another line has no launch here");
        let rendered = format!("{error:#}");
        assert!(rendered.contains(&other_line.to_string()), "{rendered}");
        assert!(
            rendered.contains(&FrameworkVersion::CURRENT.to_string()),
            "{rendered}"
        );
    }

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
