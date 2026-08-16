//! Local launch validation and the supervised bus-open boundary.
//!
//! Everything that can be decided without transport is kept ahead of
//! `BusOwner::open`: opening the bundle, reading this participant's own config
//! out of it, and validating scheduler inputs. The one thing that cannot is the
//! execution identity: it is the router's, so it is learned from the endpoints
//! the launch names rather than handed over in argv (CAMPAIGN.md, "Participant
//! process contract"). The live scheduler is intentionally built by the
//! lifecycle after the potentially slow bus connection succeeds.

use std::future::Future;
use std::time::Duration;

use crate::participant::api::Participant;
use crate::participant::clock::real::RealClock;
use crate::participant::clock::{ClockMode, ClockReading, ClockSource};
use crate::participant::launch::{SHUTDOWN_GRACE, SupervisedLaunch};
use crate::participant::scheduler::AnyStepScheduler;
use anyhow::Context as _;
use phoxal_bundle::RuntimeBundle;
use phoxal_bus::{BusConfig, BusHandle, BusOwner};
use phoxal_runtime_contract::identity::{ExecutionId, ParticipantId, TimelineId};

use super::ShutdownController;
use super::inputs::{open_bundle, participant_config};
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
    pub(crate) bundle: Option<RuntimeBundle>,
    pub(crate) config: R::Config,
    pub(crate) clock_mode: ClockMode,
    pub(crate) clock: Option<C>,
    pub(crate) query_reply_delay: Option<Duration>,
}

/// Run a participant from the strict launch contract.
pub(crate) async fn run_supervised<R, S>(launch: SupervisedLaunch, shutdown: S) -> crate::Result<()>
where
    R: Participant,
    S: Future<Output = ()>,
{
    let mut shutdown = ShutdownController::new(shutdown);
    let clock_mode = if launch.simulation {
        ClockMode::Simulation
    } else {
        ClockMode::Real
    };
    // The bundle and this participant's own config are resolved while the
    // process is still local, so a malformed manifest or a config a custom
    // `Deserialize` refuses has no producer or wire side effects to clean up.
    let bundle = open_bundle(&launch.bundle_root)?;
    let config = participant_config::<R::Config>(bundle.robot(), &launch.participant_id, R::KIND)?;

    // One line, not a per-attempt one: a participant racing a router that has
    // not opened its listener yet can take several seconds to connect. Without
    // this, that gap looks like a silent hang rather than expected startup.
    tracing::info!(
        target: "phoxal.runtime",
        endpoints = ?launch.connect_endpoints,
        "connecting to the bus"
    );
    let execution = tokio::select! {
        biased;
        _ = shutdown.wait() => return Ok(()),
        result = learn_execution(&launch.connect_endpoints) => result?,
    };
    tracing::info!(
        target: "phoxal.runtime",
        execution = %execution,
        "learned the execution identity from the router"
    );
    let clock = clock_for_mode(clock_mode, execution);
    validate_clock_inputs::<R, _>(clock_mode, clock.as_ref())?;

    let (owner, bus) = tokio::select! {
        biased;
        _ = shutdown.wait() => return Ok(()),
        result = BusOwner::open(BusConfig::for_participant(
            execution,
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
            shutdown_grace: SHUTDOWN_GRACE,
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

/// Learn the execution identity from the routers the launch points at.
///
/// A router's session id *is* the execution (`phoxal-bus`'s `probe_routers`),
/// which is why the identity is not in argv at all: the process that owns the
/// run is the one that answers on the endpoint. Exactly one is expected. Zero
/// means nothing is running there yet and there is no execution to join; more
/// than one means the endpoints named two different runs, and picking either
/// would silently attach the participant to a graph its peers are not on.
async fn learn_execution(endpoints: &[String]) -> crate::Result<ExecutionId> {
    let mut observed: Vec<ExecutionId> = Vec::new();
    for endpoint in endpoints {
        let reported = BusOwner::probe_routers(endpoint)
            .await
            .with_context(|| format!("failed to reach a Phoxal router on '{endpoint}'"))?;
        for execution in reported {
            if !observed.contains(&execution) {
                observed.push(execution);
            }
        }
    }
    match observed.as_slice() {
        [execution] => Ok(*execution),
        [] => anyhow::bail!(
            "no Phoxal router answered on {}; the execution identity is the router's, so there \
             is nothing for this participant to join",
            rendered(endpoints)
        ),
        many => anyhow::bail!(
            "the endpoints {} report {} different executions ({}); a participant joins exactly one",
            rendered(endpoints),
            many.len(),
            many.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn rendered(endpoints: &[String]) -> String {
    endpoints
        .iter()
        .map(|endpoint| format!("'{endpoint}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The real timeline of one execution.
///
/// The real-clock timeline id *is* the execution (CAMPAIGN.md, "Participant
/// process contract"), so every process in one run dates its instants on the
/// same world history without publishing anything. The two identities are
/// different widths - an execution is 128 bits, a timeline is 64 - so this
/// derives the timeline deterministically from the execution's high half rather
/// than minting an unrelated one. An execution id's most significant nibble is
/// never zero (`ExecutionId::try_from`), so that half is never zero either and
/// always names a timeline; the fallback exists only because `from_raw` is
/// total, and mints rather than panics.
fn real_timeline(execution: ExecutionId) -> TimelineId {
    let high = (u128::from(execution) >> 64) as u64;
    TimelineId::from_raw(high).unwrap_or_else(TimelineId::mint)
}

/// Build the host clock for a real launch. A simulation participant reads its
/// instants from the live world clock instead, so it gets none.
pub(crate) fn clock_for_mode(clock_mode: ClockMode, execution: ExecutionId) -> Option<RealClock> {
    match clock_mode {
        ClockMode::Real => Some(RealClock::new(real_timeline(execution))),
        ClockMode::Simulation => None,
    }
}

/// Validate scheduler selection and the initial clock discipline before any
/// supervised transport is opened. The lifecycle repeats construction after
/// the bus connects so it retains the live scheduler handle.
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

    /// The real timeline is a pure function of the execution, so two processes
    /// in one run date their instants on the same world history with nothing
    /// exchanged, and two runs never share one.
    #[test]
    fn the_real_timeline_is_derived_from_the_execution_and_nothing_else() {
        let execution = ExecutionId::mint();
        assert_eq!(real_timeline(execution), real_timeline(execution));
        assert_ne!(real_timeline(execution), real_timeline(ExecutionId::mint()));
    }

    /// Only a real launch carries a host clock; a simulated one reads the world
    /// clock the controller publishes.
    #[test]
    fn only_a_real_launch_builds_a_host_clock() {
        let execution = ExecutionId::mint();
        assert!(clock_for_mode(ClockMode::Real, execution).is_some());
        assert!(clock_for_mode(ClockMode::Simulation, execution).is_none());
    }
}
