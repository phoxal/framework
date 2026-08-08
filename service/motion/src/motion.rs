//! `motion` - the sole body-twist arbiter and emergency-stop authority.
//!
//! Manual and autonomous candidates are freshness checked, finite-value checked,
//! and clamped to the robot-authored motion limits. Motion aggregates every
//! per-component emergency-stop state before publishing `drive/target`, and the
//! safety service contributes typed world-aware constraints. Autonomous
//! candidates require fresh constraints; direct manual control may recover
//! without that provider, while still obeying e-stop, the manual lease,
//! finite-value, and robot-limit gates.
//!
//! # Emergency stop has no privileged path
//!
//! Every emergency stop is a manifest-declared `emergency_stop` component that
//! publishes state like any other component: zero declared components means
//! zero subscriptions (so a robot without e-stop hardware still supports direct
//! teleoperation), and a configured-but-silent publisher fails closed exactly
//! like any other configured input. The only thing specific to e-stop is *value
//! aggregation* - an engage observed during a cycle wins even if a release
//! follows in the same cycle - which is ordinary domain logic, not
//! communication semantics.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Result, bail};
use phoxal::api;
use phoxal::model::component::capability::Capability;
use phoxal::model::identity::CapabilityRef;
use phoxal::model::robot::MotionLimits;
use phoxal::prelude::*;

use crate::arbitration::{
    Arbitration, MANUAL_HOLD, MANUAL_SILENCE, candidate_age_ns, manual_observed_age_ns,
    safety_is_usable,
};

/// A configured component emergency stop must keep publishing. Silence past
/// this window fails closed, exactly like any other configured input.
const COMPONENT_ESTOP_STALE: Duration = Duration::from_secs(1);

/// One declared emergency stop and the subscription carrying its state.
struct BoundEmergencyStop {
    reference: CapabilityRef,
    state: Subscriber<api::component::emergency_stop::State>,
}

/// Aggregated emergency-stop state across every declared component.
///
/// Zero declared components is a valid robot: the latch is then trivially
/// clear and direct teleoperation works. Each declared component is required,
/// so a missing or stale sample blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EmergencyStopLatch {
    /// The newest engaged/released report per declared e-stop. Keying on the
    /// capability rather than on a subscriber position means a report can never
    /// be attributed to a different e-stop than the one that published it.
    component_state: BTreeMap<CapabilityRef, Option<Timed<bool>>>,
    engage_seen_this_cycle: bool,
}

impl EmergencyStopLatch {
    fn new(references: impl IntoIterator<Item = CapabilityRef>) -> Self {
        Self {
            component_state: references
                .into_iter()
                .map(|reference| (reference, None))
                .collect(),
            engage_seen_this_cycle: false,
        }
    }

    fn set_component(&mut self, reference: &CapabilityRef, engaged: bool, at: RobotInstant) {
        self.engage_seen_this_cycle |= engaged;
        if let Some(current) = self.component_state.get_mut(reference) {
            *current = Some(Timed::new(engaged, at));
        }
    }

    /// Whether any declared component blocks motion: engaged, never heard from,
    /// or silent past the staleness window.
    fn components_blocked(&self, now: RobotInstant) -> bool {
        self.component_state.values().any(|sample| {
            let Some(sample) = sample else {
                return true;
            };
            sample.body || !sample.fresh_within(now, COMPONENT_ESTOP_STALE)
        })
    }

    fn engaged(&self, now: RobotInstant) -> bool {
        self.engage_seen_this_cycle || self.components_blocked(now)
    }

    fn finish_cycle(&mut self) {
        self.engage_seen_this_cycle = false;
    }

    fn reset_timeline(&mut self) {
        // Component samples describe the replaced simulated world.
        for sample in self.component_state.values_mut() {
            *sample = None;
        }
    }
}

pub(crate) struct Api {
    manual: Subscriber<api::motion::ManualCommand>,
    autonomous: Subscriber<api::navigation::Candidate>,
    component_estops: Vec<BoundEmergencyStop>,
    safety_constraints: Subscriber<api::safety::MotionConstraints>,
    drive: CommandPublisher<api::drive::Target>,
    state: StatePublisher<api::motion::State>,
}

pub(crate) struct MotionState {
    limits: MotionLimits,
    manual: Lease<api::motion::ManualCommand>,
    manual_observed_at: Option<LocalInstant>,
    last_autonomous: Option<Timed<api::navigation::Candidate>>,
    estop: EmergencyStopLatch,
    last_safety_constraints: Option<Timed<api::safety::MotionConstraints>>,
}

#[phoxal::service(state = MotionState, api = Api)]
pub(crate) struct Motion;

impl Participant for Motion {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let robot = ctx.robot()?;
        let limits = robot.motion().limits().validate()?;
        let estops =
            robot.capability_refs(|capability| matches!(capability, Capability::EmergencyStop(_)));

        let mut component_estops = Vec::with_capacity(estops.len());
        for reference in &estops {
            component_estops.push(BoundEmergencyStop {
                reference: reference.clone(),
                state: ctx
                    .subscriber(
                        api::topic::client()
                            .component(&reference.component_id)
                            .emergency_stop(&reference.capability_id)
                            .state(),
                        32,
                    )
                    .await?,
            });
        }

        Ok((
            MotionState {
                limits,
                manual: Lease::new("motion/manual", MANUAL_SILENCE, MANUAL_HOLD),
                manual_observed_at: None,
                last_autonomous: None,
                estop: EmergencyStopLatch::new(estops),
                last_safety_constraints: None,
            },
            Api {
                manual: ctx
                    .subscriber(api::topic::owner().motion().manual(), 32)
                    .await?,
                autonomous: ctx
                    .subscriber(api::topic::client().navigation().candidate(), 32)
                    .await?,
                component_estops,
                safety_constraints: ctx
                    .subscriber(api::topic::client().safety().constraints(), 32)
                    .await?,
                drive: ctx
                    .command_publisher(api::topic::client().drive().target())
                    .await?,
                state: ctx
                    .state_publisher(api::topic::owner().motion().state())
                    .await?,
            },
        ))
    }

    async fn reset(
        &self,
        _ctx: ResetContext,
        _api: &Self::Api,
        state: &mut Self::State,
    ) -> Result<()> {
        state.last_autonomous = None;
        state.last_safety_constraints = None;
        state.estop.reset_timeline();
        // The manual command is a clockless operator input sampled at a logical
        // step, not state derived from the replaced world, so the lease is not
        // cleared here and keeps ageing on the host clock across the boundary.
        // What happens to a held command depends on whether it had already been
        // applied: one anchored on the retired timeline is dropped at the first
        // step of the replacement, because its horizon cannot be measured
        // across worlds; one that arrived but has not yet been applied anchors
        // on that first step instead. Either way the machine keeps moving only
        // while the operator keeps publishing.
        Ok(())
    }

    #[phoxal::step(hz = 20)]
    async fn step(
        &self,
        api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        let now = step.now();
        // Without the host clock there is no silence deadline to measure, so
        // this step decides nothing: it renews no lease and applies no
        // command, and the leases expire on their own. The runner's own clock
        // read faults the participant on the same step.
        let Some(host_now) = LocalInstant::try_now() else {
            bail!("the host boot clock could not be read");
        };

        while let Some(observed) = api.manual.try_recv() {
            let observed_at = observed.observed_at;
            match state.manual.offer(
                observed.metadata.producer,
                observed.metadata.sequence,
                observed_at,
                observed.body,
            ) {
                LeaseDecision::Rejected(rejection) => {
                    tracing::warn!(
                        target: "phoxal.motion",
                        error = %rejection,
                        "rejected manual command"
                    );
                }
                _ => state.manual_observed_at = Some(observed_at),
            }
        }
        while let Some(observed) = api.autonomous.try_recv() {
            if let Some(at) = observed.metadata.produced_exactly_at() {
                state.last_autonomous = Some(Timed::new(observed.body, at));
            }
        }
        for bound in &api.component_estops {
            while let Some(observed) = bound.state.try_recv() {
                if let Some(at) = observed.metadata.produced_exactly_at() {
                    state
                        .estop
                        .set_component(&bound.reference, observed.body.engaged, at);
                }
            }
        }
        while let Some(observed) = api.safety_constraints.try_recv() {
            if let Some(at) = observed.metadata.produced_exactly_at() {
                state.last_safety_constraints = Some(Timed::new(observed.body, at));
            }
        }

        let safety_runtime = if state
            .last_safety_constraints
            .as_ref()
            .is_some_and(|constraints| safety_is_usable(constraints, now))
        {
            api::motion::SafetyRuntime::Present
        } else {
            api::motion::SafetyRuntime::Absent
        };
        let manual = state.manual.live(host_now, now).cloned();
        if manual.is_none() {
            state.manual_observed_at = None;
        }
        let arbitration = Arbitration::decide(
            manual.as_ref(),
            state.last_autonomous.as_ref(),
            state.estop.engaged(now),
            state.last_safety_constraints.as_ref(),
            state.limits,
            now,
        );
        state.estop.finish_cycle();

        api.drive.send(arbitration.selected.clone())?;
        api.state.publish(
            &step.token,
            api::motion::State {
                manual_observed_age_ns: manual_observed_age_ns(state.manual_observed_at, host_now),
                autonomous_candidate_age_ns: candidate_age_ns(state.last_autonomous.as_ref(), now),
                safety_constraints_age_ns: candidate_age_ns(
                    state.last_safety_constraints.as_ref(),
                    now,
                ),
                selected_source: arbitration.source,
                final_target: state_target(&arbitration.selected),
                zero_reason: arbitration.zero_reason,
                safety_runtime,
                component_estop_blocked: state.estop.components_blocked(now),
                active_safety_constraints: state.last_safety_constraints.as_ref().map_or_else(
                    Vec::new,
                    |constraints| {
                        if safety_is_usable(constraints, now) {
                            constraints.body.constraints.clone()
                        } else {
                            Vec::new()
                        }
                    },
                ),
            },
        )?;
        Ok(())
    }
}

fn state_target(target: &api::drive::Target) -> api::motion::Target {
    api::motion::Target {
        linear_x_mps: target.linear_x_mps,
        angular_z_radps: target.angular_z_radps,
        curvature_limit_radpm: target.curvature_limit_radpm,
    }
}

#[cfg(test)]
mod tests {
    use phoxal::bus::{ProducerId, TimelineId};

    use super::*;

    /// A distinct test producer. Nothing mints a producer in production - a
    /// session's identity is the session - so tests name theirs explicitly.
    fn producer(value: u128) -> ProducerId {
        ProducerId::try_from(value).expect("a test producer is nonzero")
    }

    fn line() -> TimelineId {
        TimelineId::from_raw(1).expect("test timeline must be nonzero")
    }

    fn at(ticks: u64) -> RobotInstant {
        RobotInstant::new(line(), ticks)
    }

    fn estop(index: u8) -> CapabilityRef {
        format!("estop{index}.stop")
            .parse()
            .expect("a test e-stop reference is well formed")
    }

    fn latch(count: u8) -> EmergencyStopLatch {
        EmergencyStopLatch::new((0..count).map(estop))
    }

    #[test]
    fn state_target_preserves_the_drive_command() {
        let drive = api::drive::Target {
            linear_x_mps: 0.3,
            angular_z_radps: -0.4,
            curvature_limit_radpm: Some(1.0),
        };
        let state = state_target(&drive);
        assert_eq!(state.linear_x_mps, drive.linear_x_mps);
        assert_eq!(state.angular_z_radps, drive.angular_z_radps);
        assert_eq!(state.curvature_limit_radpm, drive.curvature_limit_radpm);
    }

    /// The manual command runs on the same receiver-owned lease as any other
    /// leased input, so it needs no bespoke path: silence on the host clock and
    /// travel on robot time each expire it independently.
    #[test]
    fn the_manual_lease_expires_on_host_silence_and_on_the_logical_horizon() {
        let command = api::motion::ManualCommand {
            linear_x_mps: 0.4,
            angular_z_radps: 0.0,
        };
        let producer = producer(1);
        let host_start = LocalInstant::from_boot_ns(0);

        let mut silent = Lease::new("motion/manual", MANUAL_SILENCE, MANUAL_HOLD);
        silent.offer(producer, 1, host_start, command.clone());
        assert!(silent.live(host_start, at(0)).is_some());
        assert!(
            silent
                .live(
                    host_start.saturating_add(MANUAL_SILENCE + Duration::from_millis(1)),
                    at(0)
                )
                .is_none()
        );

        let mut held = Lease::new("motion/manual", MANUAL_SILENCE, MANUAL_HOLD);
        held.offer(producer, 1, host_start, command);
        assert!(held.live(host_start, at(0)).is_some());
        let past_hold = u64::try_from(MANUAL_HOLD.as_nanos()).unwrap() + 1;
        assert!(held.live(host_start, at(past_hold)).is_none());
    }

    /// A lease that expired while the simulation was paused must not apply on
    /// the first resumed arbitration step.
    #[test]
    fn a_lease_expired_while_paused_does_not_apply_on_the_first_resumed_step() {
        let producer = producer(2);
        let host_start = LocalInstant::from_boot_ns(0);
        let mut lease = Lease::new("motion/manual", MANUAL_SILENCE, MANUAL_HOLD);
        lease.offer(
            producer,
            1,
            host_start,
            api::motion::ManualCommand {
                linear_x_mps: 0.4,
                angular_z_radps: 0.0,
            },
        );
        assert!(lease.live(host_start, at(0)).is_some());

        // Steps stop while host time keeps running past the silence deadline.
        let resumed = host_start.saturating_add(MANUAL_SILENCE + Duration::from_millis(1));
        assert!(lease.live(resumed, at(1)).is_none());
    }

    #[test]
    fn component_estops_latch_independently_and_release_together() {
        let mut latch = latch(2);
        let now = at(1_000);
        assert!(
            latch.engaged(now),
            "configured component e-stops must publish before motion"
        );
        latch.set_component(&estop(0), false, now);
        latch.set_component(&estop(1), false, now);
        latch.finish_cycle();
        assert!(!latch.engaged(now));

        latch.set_component(&estop(1), true, now);
        latch.finish_cycle();
        assert!(latch.engaged(now));
        latch.set_component(&estop(1), false, now);
        latch.finish_cycle();
        assert!(!latch.engaged(now));
    }

    /// Value aggregation: an engage observed during a cycle wins even if a
    /// release follows in the same cycle. This is domain logic, not a
    /// privileged communication path, so it is exercised through an ordinary
    /// component sample.
    #[test]
    fn engage_then_release_in_one_cycle_still_forces_a_stop_cycle() {
        let mut latch = latch(1);
        let now = at(1_000);
        latch.set_component(&estop(0), true, now);
        latch.set_component(&estop(0), false, now);
        assert!(latch.engaged(now));
        latch.finish_cycle();
        assert!(!latch.engaged(now));
    }

    #[test]
    fn a_robot_without_component_estops_starts_ready_for_manual_control() {
        let latch = latch(0);
        assert!(
            !latch.engaged(at(1_000)),
            "zero declared components means zero subscriptions, so direct teleoperation is valid"
        );
    }

    #[test]
    fn missing_stale_future_and_replaced_timeline_component_estops_fail_closed() {
        let stale_ns = u64::try_from(COMPONENT_ESTOP_STALE.as_nanos()).unwrap();
        let now = at(stale_ns + 10);
        let mut latch = latch(1);
        assert!(
            latch.components_blocked(now),
            "a missing sample fails closed"
        );

        latch.set_component(&estop(0), false, at(9));
        assert!(latch.components_blocked(now), "a stale sample fails closed");
        latch.set_component(&estop(0), false, at(now.ticks() + 1));
        assert!(
            latch.components_blocked(now),
            "a sample from this step's future fails closed"
        );
        latch.set_component(
            &estop(0),
            false,
            RobotInstant::new(TimelineId::mint(), now.ticks()),
        );
        assert!(
            latch.components_blocked(now),
            "a sample from a replaced world is incomparable, so it fails closed"
        );
        latch.set_component(&estop(0), false, now);
        latch.finish_cycle();
        assert!(!latch.components_blocked(now));
    }

    #[test]
    fn a_replaced_timeline_clears_component_samples_so_they_must_republish() {
        let mut latch = latch(1);
        let now = at(1_000);
        latch.set_component(&estop(0), false, now);
        latch.finish_cycle();
        assert!(!latch.engaged(now));

        latch.reset_timeline();
        assert!(
            latch.engaged(now),
            "after a world replacement every configured component must publish again"
        );
    }
}
