//! `motion` - the sole body-twist arbiter and emergency-stop authority.
//!
//! Manual and autonomous candidates are freshness checked, finite-value checked,
//! and clamped to the robot-authored motion limits. Motion aggregates every
//! per-component emergency-stop state before publishing `drive/target`, and the
//! redesigned safety service contributes typed world-aware constraints.
//! Autonomous candidates require fresh constraints; direct manual control may
//! recover without that experimental provider, while still obeying e-stop, the
//! manual lease, finite-value, and robot-limit gates.
//!
//! # Emergency stop has no privileged path (#952 section A)
//!
//! There is no software emergency-stop contract and no bespoke latch for one.
//! Every emergency stop is a manifest-declared `emergency_stop` component that
//! publishes state like any other component: zero declared components means
//! zero subscriptions (so a robot without e-stop hardware still supports direct
//! teleoperation), and a configured-but-silent publisher fails closed exactly
//! like any other configured input. The only thing that remains specific to
//! e-stop is *value aggregation* - an engage observed during a cycle wins even
//! if a release follows in the same cycle - which is ordinary domain logic, not
//! communication semantics.

use std::time::Duration;

use anyhow::{Result, bail};
use phoxal::api;
use phoxal::model::component::v0::capability::Capability;
use phoxal::model::robot::v0::MotionLimits;
use phoxal::model::v0::Robot;
use phoxal::prelude::*;

use crate::arbitration::{
    MANUAL_HOLD, MANUAL_SILENCE, Timed, arbitrate, candidate_age_ns, manual_observed_age_ns,
    safety_is_usable,
};

/// A configured component emergency stop must keep publishing. Silence past
/// this window fails closed, exactly like any other configured input.
const COMPONENT_ESTOP_STALE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
struct EmergencyStopBinding {
    component_id: String,
    capability_id: String,
}

/// Aggregated emergency-stop state across every declared component.
///
/// Zero declared components is a valid robot: the latch is then trivially
/// clear and direct teleoperation works. Each declared component is required,
/// so a missing or stale sample blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EmergencyStopLatch {
    component_state: Vec<Option<(bool, RobotInstant)>>,
    engage_seen_this_cycle: bool,
}

impl EmergencyStopLatch {
    fn new(component_count: usize) -> Self {
        Self {
            component_state: vec![None; component_count],
            engage_seen_this_cycle: false,
        }
    }

    fn set_component(&mut self, index: usize, engaged: bool, at: RobotInstant) {
        self.engage_seen_this_cycle |= engaged;
        if let Some(current) = self.component_state.get_mut(index) {
            *current = Some((engaged, at));
        }
    }

    /// Whether any declared component blocks motion: engaged, never heard from,
    /// or silent past the staleness window.
    fn components_blocked(&self, now: RobotInstant) -> bool {
        self.component_state.iter().any(|sample| {
            let Some((engaged, at)) = sample else {
                return true;
            };
            *engaged
                || !TimeWindow::exact(*at)
                    .possibly_fresh_within(now, COMPONENT_ESTOP_STALE)
                    .unwrap_or(false)
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
        self.component_state.fill(None);
    }
}

#[derive(phoxal::Api)]
pub struct Api {
    manual: Subscriber<api::motion::ManualCommand>,
    autonomous: Subscriber<api::navigation::Candidate>,
    component_estops: Vec<Subscriber<api::component::emergency_stop::State>>,
    safety_constraints: Subscriber<api::safety::MotionConstraints>,
    drive: CommandPublisher<api::drive::Target>,
    state: StatePublisher<api::motion::State>,
}

#[phoxal::service(id = "motion", config = ())]
pub struct Motion {
    limits: MotionLimits,
    manual: Lease<api::motion::ManualCommand>,
    manual_observed_at: Option<LocalInstant>,
    last_autonomous: Option<Timed<api::navigation::Candidate>>,
    estop: EmergencyStopLatch,
    last_safety_constraints: Option<Timed<api::safety::MotionConstraints>>,
}

#[phoxal::behavior]
impl Motion {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let robot = ctx.robot()?;
        let limits = robot.manifest.robot.motion_limits.validate()?;
        let estop_bindings = emergency_stop_bindings(robot);

        let mut component_estops = Vec::with_capacity(estop_bindings.len());
        for binding in &estop_bindings {
            component_estops.push(
                ctx.subscriber(
                    api::topic::client()
                        .component(&binding.component_id)
                        .emergency_stop(&binding.capability_id)
                        .state(),
                    32,
                )
                .await?,
            );
        }

        Ok((
            Self {
                limits,
                manual: Lease::new("motion/manual", MANUAL_SILENCE, MANUAL_HOLD),
                manual_observed_at: None,
                last_autonomous: None,
                estop: EmergencyStopLatch::new(estop_bindings.len()),
                last_safety_constraints: None,
            },
            Self::Api {
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

    #[reset]
    async fn reset(&mut self, _ctx: ResetContext) -> Result<()> {
        self.last_autonomous = None;
        self.last_safety_constraints = None;
        self.estop.reset_timeline();
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

    #[step(hz = 20)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
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
            match self.manual.offer(
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
                _ => self.manual_observed_at = Some(observed_at),
            }
        }
        while let Some(observed) = api.autonomous.try_recv() {
            if let Some(at) = observed.metadata.produced_exactly_at() {
                self.last_autonomous = Some(Timed {
                    body: observed.body,
                    at,
                });
            }
        }
        for (index, subscriber) in api.component_estops.iter().enumerate() {
            while let Some(observed) = subscriber.try_recv() {
                if let Some(at) = observed.metadata.produced_exactly_at() {
                    self.estop.set_component(index, observed.body.engaged, at);
                }
            }
        }
        while let Some(observed) = api.safety_constraints.try_recv() {
            if let Some(at) = observed.metadata.produced_exactly_at() {
                self.last_safety_constraints = Some(Timed {
                    body: observed.body,
                    at,
                });
            }
        }

        let safety_runtime = if self
            .last_safety_constraints
            .as_ref()
            .is_some_and(|constraints| safety_is_usable(constraints, now))
        {
            api::motion::SafetyRuntime::Present
        } else {
            api::motion::SafetyRuntime::Absent
        };
        let manual = self.manual.live(host_now, now).cloned();
        if manual.is_none() {
            self.manual_observed_at = None;
        }
        let arbitration = arbitrate(
            manual.as_ref(),
            self.last_autonomous.as_ref(),
            self.estop.engaged(now),
            self.last_safety_constraints.as_ref(),
            self.limits,
            now,
        );
        self.estop.finish_cycle();

        api.drive.send(arbitration.selected.clone())?;
        api.state.publish(
            step.token(),
            api::motion::State {
                manual_observed_age_ns: manual_observed_age_ns(self.manual_observed_at, host_now),
                autonomous_candidate_age_ns: candidate_age_ns(self.last_autonomous.as_ref(), now),
                safety_constraints_age_ns: candidate_age_ns(
                    self.last_safety_constraints.as_ref(),
                    now,
                ),
                selected_source: arbitration.source,
                final_target: state_target(&arbitration.selected),
                zero_reason: arbitration.zero_reason,
                safety_runtime,
                component_estop_blocked: self.estop.components_blocked(now),
                active_safety_constraints: self.last_safety_constraints.as_ref().map_or_else(
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

fn emergency_stop_bindings(robot: &Robot) -> Vec<EmergencyStopBinding> {
    let mut bindings = robot
        .manifest
        .components()
        .iter()
        .filter_map(|(component_id, instance)| {
            robot
                .components
                .get(&instance.component)
                .map(|component| (component_id, component))
        })
        .flat_map(|(component_id, component)| {
            component
                .capabilities
                .iter()
                .filter(|(_, capability)| matches!(capability, Capability::EmergencyStop(_)))
                .map(|(capability_id, _)| EmergencyStopBinding {
                    component_id: component_id.clone(),
                    capability_id: capability_id.clone(),
                })
        })
        .collect::<Vec<_>>();
    bindings.sort_by(|left, right| {
        left.component_id
            .cmp(&right.component_id)
            .then_with(|| left.capability_id.cmp(&right.capability_id))
    });
    bindings
}

#[cfg(test)]
mod tests {
    use phoxal::bus::{ContractBody, ProducerId, TimelineId};
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};

    use super::*;

    fn line() -> TimelineId {
        TimelineId::from_raw(1).expect("test timeline must be nonzero")
    }

    fn at(ticks: u64) -> RobotInstant {
        RobotInstant::new(line(), ticks)
    }

    #[test]
    fn api_owns_estop_and_state_and_consumes_component_estops() {
        assert_eq!(<Motion as Participant>::ID, "motion");
        let contracts = <<Motion as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert_contract::<api::motion::ManualCommand>(contracts, ContractRole::Subscribe);
        assert_contract::<api::navigation::Candidate>(contracts, ContractRole::Subscribe);
        assert_contract::<api::component::emergency_stop::State>(
            contracts,
            ContractRole::Subscribe,
        );
        assert_contract::<api::drive::Target>(contracts, ContractRole::Publish);
        assert_contract::<api::motion::State>(contracts, ContractRole::Publish);
        assert_contract::<api::safety::MotionConstraints>(contracts, ContractRole::Subscribe);
    }

    fn assert_contract<B>(contracts: &[phoxal::participant::ApiContractUse], role: ContractRole)
    where
        B: ContractBody,
    {
        assert!(
            contracts
                .iter()
                .any(|contract| contract.topic == B::TOPIC && contract.role == role)
        );
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
        let producer = ProducerId::mint();
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
        let producer = ProducerId::mint();
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
        let mut latch = EmergencyStopLatch::new(2);
        let now = at(1_000);
        assert!(
            latch.engaged(now),
            "configured component e-stops must publish before motion"
        );
        latch.set_component(0, false, now);
        latch.set_component(1, false, now);
        latch.finish_cycle();
        assert!(!latch.engaged(now));

        latch.set_component(1, true, now);
        latch.finish_cycle();
        assert!(latch.engaged(now));
        latch.set_component(1, false, now);
        latch.finish_cycle();
        assert!(!latch.engaged(now));
    }

    /// Preserved value aggregation: an engage observed during a cycle wins even
    /// if a release follows in the same cycle. This is domain logic, not a
    /// privileged communication path - it is exercised with two component
    /// samples, since there is no software e-stop any more.
    #[test]
    fn engage_then_release_in_one_cycle_still_forces_a_stop_cycle() {
        let mut latch = EmergencyStopLatch::new(1);
        let now = at(1_000);
        latch.set_component(0, true, now);
        latch.set_component(0, false, now);
        assert!(latch.engaged(now));
        latch.finish_cycle();
        assert!(!latch.engaged(now));
    }

    #[test]
    fn a_robot_without_component_estops_starts_ready_for_manual_control() {
        let latch = EmergencyStopLatch::new(0);
        assert!(
            !latch.engaged(at(1_000)),
            "zero declared components means zero subscriptions, so direct teleoperation is valid"
        );
    }

    #[test]
    fn missing_stale_future_and_replaced_timeline_component_estops_fail_closed() {
        let stale_ns = u64::try_from(COMPONENT_ESTOP_STALE.as_nanos()).unwrap();
        let now = at(stale_ns + 10);
        let mut latch = EmergencyStopLatch::new(1);
        assert!(
            latch.components_blocked(now),
            "a missing sample fails closed"
        );

        latch.set_component(0, false, at(9));
        assert!(latch.components_blocked(now), "a stale sample fails closed");
        latch.set_component(0, false, at(now.ticks() + 1));
        assert!(
            latch.components_blocked(now),
            "a sample from this step's future fails closed"
        );
        latch.set_component(0, false, RobotInstant::new(TimelineId::mint(), now.ticks()));
        assert!(
            latch.components_blocked(now),
            "a sample from a replaced world is incomparable, so it fails closed"
        );
        latch.set_component(0, false, now);
        latch.finish_cycle();
        assert!(!latch.components_blocked(now));
    }

    #[test]
    fn a_replaced_timeline_clears_component_samples_so_they_must_republish() {
        let mut latch = EmergencyStopLatch::new(1);
        let now = at(1_000);
        latch.set_component(0, false, now);
        latch.finish_cycle();
        assert!(!latch.engaged(now));

        latch.reset_timeline();
        assert!(
            latch.engaged(now),
            "after a world replacement every configured component must publish again"
        );
    }
}
