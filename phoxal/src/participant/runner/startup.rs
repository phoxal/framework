//! Local launch validation and the supervised bus-open boundary.
//!
//! Attachment starts from one rendezvous endpoint. The framework resolves the
//! execution, opens its caller-owned bus session, completes the supervisor
//! bootstrap, and only then validates the participant's role and configuration.
//! The live scheduler is intentionally built by the lifecycle after the
//! potentially slow connection succeeds.

use std::future::Future;
use std::time::Duration;

use crate::bus::{BusConfig, BusHandle, BusOwner};
use crate::execution::{attach_execution, resolve_execution};
use crate::identity::{ParticipantId, TimelineId};
use crate::participant::api::Participant;
use crate::participant::clock::real::RealClock;
use crate::participant::clock::{ClockMode, ClockReading, ClockSource};
use crate::participant::context::SetupSource;
use crate::participant::launch::{Launch, SHUTDOWN_GRACE};
use crate::participant::metadata::ParticipantKind;
use crate::participant::scheduler::AnyStepScheduler;
use crate::supervisor::api::time_domain::{TimeDomain, TimeMode};
use anyhow::Context as _;

use super::ShutdownController;
use super::inputs::{driver_block, participant_config};
use super::lifecycle::{self, BusLease};

/// The supervisor's initial scheduling authority plus its already-subscribed
/// replacement stream. Only services and the brain retain this; drivers are
/// deliberately independent of execution time mode.
pub(crate) struct DomainSubscription {
    pub(crate) current: TimeDomain,
    pub(crate) updates:
        crate::bus::StreamReceiver<crate::supervisor::api::time_domain::TimeDomainStream>,
}

impl DomainSubscription {
    /// Reconcile each replacement buffered before the next lifecycle boundary.
    ///
    /// Later arrivals remain in the ordered stream for the runner's serialized
    /// event loop, so this establishes an initial domain without creating a
    /// receive gap.
    pub(crate) fn reconcile(&mut self) -> crate::Result<Vec<(TimeDomain, TimeDomain)>> {
        let mut replacements = Vec::new();
        while let Some(update) = self.updates.try_recv()? {
            if update.body.domain.revision > self.current.revision {
                let previous = self.current;
                self.current = update.body.domain;
                replacements.push((previous, self.current));
            }
        }
        Ok(replacements)
    }

    /// Wait for the next strictly newer scheduling authority.
    pub(crate) async fn next_replacement(&mut self) -> crate::Result<(TimeDomain, TimeDomain)> {
        loop {
            let update = self.updates.recv().await?.body.domain;
            if update.revision > self.current.revision {
                let previous = self.current;
                self.current = update;
                return Ok((previous, update));
            }
        }
    }
}

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
    pub(crate) source: SetupSource,
    pub(crate) domain: Option<DomainSubscription>,
    pub(crate) config: R::Config,
    pub(crate) clock_mode: ClockMode,
    pub(crate) clock: Option<C>,
    pub(crate) query_reply_delay: Option<Duration>,
}

/// Run a participant from the strict launch contract.
pub(crate) async fn run_supervised<R, S>(launch: Launch, shutdown: S) -> crate::Result<()>
where
    R: Participant,
    S: Future<Output = ()>,
{
    let mut shutdown = ShutdownController::new(shutdown);
    // One line, not a per-attempt one: a participant racing a router that has
    // not opened its listener yet can take several seconds to connect. Without
    // this, that gap looks like a silent hang rather than expected startup.
    tracing::info!(
        target: "phoxal.runtime",
        endpoint = %launch.connect,
        "connecting to the bus"
    );
    let execution = tokio::select! {
        biased;
        _ = shutdown.wait() => return Ok(()),
        result = resolve_execution(&launch.connect) => result?,
    };
    tracing::info!(
        target: "phoxal.runtime",
        execution = %execution,
        "learned the execution identity from the router"
    );
    let (owner, bus) = tokio::select! {
        biased;
        _ = shutdown.wait() => return Ok(()),
        result = BusOwner::open(BusConfig::for_participant(
            execution,
            launch.participant_id.clone(),
            vec![launch.connect.clone()],
        )) => result?,
    };

    let bootstrap = match tokio::select! {
        biased;
        _ = shutdown.wait() => {
            let _ = owner.close().await;
            return Ok(());
        }
        result = attach_execution(&bus) => result,
    } {
        Ok(bootstrap) => bootstrap,
        Err(error) => {
            let _ = owner.close().await;
            return Err(error.into());
        }
    };
    let preflight = async {
        let robot = bootstrap.info.manifest.into_robot();
        let config = participant_config::<R>(&robot, &launch.participant_id)?;
        validate_declared_connection::<R>(&robot, &launch.participant_id)?;
        let assets = crate::bundle::ParticipantAssets::from_supervisor(bus.clone())?;
        let clock_mode = clock_mode_for::<R>(bootstrap.time_domain);
        let clock = clock_for_mode(clock_mode, bootstrap.time_domain.timeline);
        validate_clock_inputs::<R, _>(clock_mode, clock.as_ref())?;
        Ok::<_, anyhow::Error>((robot, config, assets, clock_mode, clock))
    };
    let (robot, config, assets, clock_mode, clock) = match tokio::select! {
        biased;
        _ = shutdown.wait() => {
            let _ = owner.close().await;
            return Ok(());
        }
        result = preflight => result,
    } {
        Ok(preflight) => preflight,
        Err(error) => {
            let _ = owner.close().await;
            return Err(error);
        }
    };

    // Do not construct the live scheduler until this connection boundary has
    // completed. The preflight above validates its inputs without creating a
    // scheduler that could outlive a failed or cancelled bus open.
    lifecycle::run(
        PreparedRun::<R, RealClock> {
            bus,
            session: BusLease::Owned(owner),
            participant_id: launch.participant_id,
            shutdown_grace: SHUTDOWN_GRACE,
            source: SetupSource::Supervisor {
                robot: Box::new(robot),
                assets,
            },
            domain: (R::KIND != ParticipantKind::Driver).then_some(DomainSubscription {
                current: bootstrap.time_domain,
                updates: bootstrap.time_domains,
            }),
            config,
            clock_mode,
            clock,
            query_reply_delay: None,
        },
        &mut shutdown,
    )
    .await
}

/// Select the initial participant cadence from supervisor authority.
///
/// Drivers are deliberately outside this decision: their host-local cadence is
/// independent of a world attaching, pausing, or resetting.
fn clock_mode_for<R: Participant>(domain: TimeDomain) -> ClockMode {
    if R::KIND == ParticipantKind::Driver || domain.mode == TimeMode::Monotonic {
        ClockMode::Real
    } else {
        ClockMode::Simulation
    }
}

/// Build the host clock for one supervisor-minted monotonic timeline. A
/// simulated service or brain reads its instants from the live world clock.
pub(crate) fn clock_for_mode(clock_mode: ClockMode, timeline: TimelineId) -> Option<RealClock> {
    match clock_mode {
        ClockMode::Real => Some(RealClock::new(timeline)),
        ClockMode::Simulation => None,
    }
}

/// Refuse an authored connection this driver does not accept after supervisor
/// attachment but before the participant becomes Ready.
///
/// `phoxal validate` is what an author actually meets this rule through:
/// it reads the same declaration out of the built binary's embedded metadata
/// and compares it against the document, so a mismatch is a build-time failure
/// with the document in hand. This is the defence in depth behind it - the
/// binary refusing to drive hardware it was not written for - and it completes
/// before setup, query declaration, or Ready acquisition.
///
/// A role that declares no kind, and every role that is not a driver, states
/// `CONNECTION = None` and has nothing to check.
fn validate_declared_connection<R: Participant>(
    robot: &crate::model::Robot,
    participant_id: &ParticipantId,
) -> crate::Result<()> {
    let Some(expected) = R::CONNECTION else {
        return Ok(());
    };
    let authored = driver_block(robot, participant_id)?.connection().kind();
    anyhow::ensure!(
        authored == expected,
        "driver '{participant_id}' accepts a {expected} connection, but the component instance \
         it is launched for authors a {authored} connection"
    );
    Ok(())
}

/// Validate scheduler selection and initial clock discipline after supervisor
/// attachment and before the lifecycle constructs its retained scheduler.
pub(crate) fn validate_clock_inputs<R, C>(
    clock_mode: ClockMode,
    clock: Option<&C>,
) -> crate::Result<()>
where
    R: Participant,
    C: ClockSource,
{
    let now = match clock_mode {
        ClockMode::Real => {
            let reading = clock
                .context("a real participant is launched with a host clock")?
                .read();
            match reading {
                ClockReading::Synchronized(_) => reading.instant(),
                ClockReading::Unsynchronized(reason) => {
                    return Err(lifecycle::ClockDisciplineLost { reason }.into());
                }
            }
        }
        ClockMode::Simulation => None,
    };
    AnyStepScheduler::validate_clock_mode(clock_mode, R::__step_schedule(), now)
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::inputs::open_bundle;
    use crate::participant::context::SetupContext;
    use phoxal_fixture::staged_bundle;

    /// The fixture's drive motors are authored with a CAN connection, so a
    /// driver declaring `can` matches them and one declaring `serial` does not.
    #[phoxal::driver(id = "front_left_drive", connection = can)]
    struct CanDriver;

    impl Participant for CanDriver {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> crate::Result<(Self::State, Self::Api)> {
            Ok(((), ()))
        }
    }

    #[phoxal::driver(id = "front_left_drive", connection = serial)]
    struct SerialDriver;

    impl Participant for SerialDriver {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> crate::Result<(Self::State, Self::Api)> {
            Ok(((), ()))
        }
    }

    #[phoxal::driver(id = "front_left_drive")]
    struct AnyDriver;

    impl Participant for AnyDriver {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> crate::Result<(Self::State, Self::Api)> {
            Ok(((), ()))
        }
    }

    /// The declared kind is enforced before the participant becomes Ready, so
    /// a binary wired to hardware it was not written for cannot serve it and a
    /// driver that declared nothing is not held to a kind it never named.
    #[test]
    fn a_declared_connection_kind_is_enforced_before_ready() {
        let staged = staged_bundle();
        let bundle = open_bundle(staged.path()).expect("the staged bundle opens");
        let robot = bundle.robot();
        let id = ParticipantId::new("front_left_drive").expect("a test participant id");

        validate_declared_connection::<CanDriver>(robot, &id)
            .expect("the authored kind is the declared one");
        validate_declared_connection::<AnyDriver>(robot, &id)
            .expect("a driver that declared no kind accepts the authored one");

        let error = validate_declared_connection::<SerialDriver>(robot, &id)
            .expect_err("an authored kind the driver does not accept must be refused");
        let message = format!("{error:#}");
        for expected in ["front_left_drive", "serial", "can"] {
            assert!(message.contains(expected), "{message}");
        }
    }

    /// The supervisor mints the monotonic timeline before participants attach,
    /// so every real scheduler uses that same opaque authority value.
    #[test]
    fn the_real_scheduler_uses_the_supervisor_timeline() {
        let timeline = TimelineId::from_raw(7).expect("a test timeline");
        let clock = clock_for_mode(ClockMode::Real, timeline).expect("a real clock");
        assert_eq!(
            clock.read().instant().expect("host clock reads").timeline(),
            timeline
        );
    }

    /// Only the real clock mode carries a host clock; the simulated mode reads
    /// the world clock the controller publishes.
    #[test]
    fn only_the_real_clock_mode_builds_a_host_clock() {
        let timeline = TimelineId::from_raw(9).expect("a test timeline");
        assert!(clock_for_mode(ClockMode::Real, timeline).is_some());
        assert!(clock_for_mode(ClockMode::Simulation, timeline).is_none());
    }
}
