//! `motion` - the sole body-twist arbiter and emergency-stop authority.
//!
//! Manual and autonomous candidates are freshness checked, finite-value checked,
//! and clamped to the robot-authored motion limits. With the legacy safety
//! runtime removed, motion also aggregates the software emergency stop and every
//! per-component emergency-stop state before publishing `drive/target`. The
//! redesigned safety service contributes typed world-aware constraints.
//! Autonomous candidates require fresh constraints; direct manual control may
//! recover without that experimental provider, while still obeying e-stop,
//! freshness, finite-value, and robot-limit gates.

mod arbitration;

use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use phoxal::api;
use phoxal::model::component::v0::capability::Capability;
use phoxal::model::robot::v0::MotionLimits;
use phoxal::model::v0::Robot;
use phoxal::prelude::*;

use crate::arbitration::{
    ManualCandidate, Timed, arbitrate, candidate_age_ns, manual_candidate_age_ns, safety_is_usable,
};

const COMPONENT_ESTOP_STALE_NS: u64 = 1_000_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct EmergencyStopBinding {
    component_id: String,
    capability_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EmergencyStopLatch {
    software_engaged: Option<bool>,
    component_state: Vec<Option<(bool, LogicalTime)>>,
    engage_seen_this_cycle: bool,
}

impl EmergencyStopLatch {
    fn new(component_count: usize) -> Self {
        Self {
            // Software e-stop is a live command latch, not a startup permit.
            // Starting clear keeps direct joypad teleoperation usable without
            // a separate behavior/operator command. Any configured physical
            // component e-stop still fails closed until it publishes fresh
            // state, and an engaged software request still wins immediately.
            software_engaged: Some(false),
            component_state: vec![None; component_count],
            engage_seen_this_cycle: false,
        }
    }

    fn set_software(&mut self, engaged: bool) {
        self.engage_seen_this_cycle |= engaged;
        self.software_engaged = Some(engaged);
    }

    fn set_component(&mut self, index: usize, engaged: bool, at: LogicalTime) {
        self.engage_seen_this_cycle |= engaged;
        if let Some(current) = self.component_state.get_mut(index) {
            *current = Some((engaged, at));
        }
    }

    fn software_blocked(&self) -> bool {
        self.engage_seen_this_cycle || self.software_engaged != Some(false)
    }

    fn components_blocked(&self, now: LogicalTime) -> bool {
        self.component_state.iter().any(|sample| {
            let Some((engaged, at)) = sample else {
                return true;
            };
            *engaged
                || at.epoch() != now.epoch()
                || at.time_ns() > now.time_ns()
                || now.time_ns().saturating_sub(at.time_ns()) > COMPONENT_ESTOP_STALE_NS
        })
    }

    fn engaged(&self, now: LogicalTime) -> bool {
        self.software_blocked() || self.components_blocked(now)
    }

    fn finish_cycle(&mut self) {
        self.engage_seen_this_cycle = false;
    }
}

#[derive(phoxal::Api)]
struct Api {
    // Declares the typed graph subscription. The managed receiver owns the
    // only draining clone so logical-step pauses cannot accumulate a replay
    // backlog in this handle.
    manual: Subscriber<api::motion::ManualCommand>,
    autonomous: Subscriber<api::navigation::Candidate>,
    software_estop: Subscriber<api::motion::EmergencyStopRequest>,
    component_estops: Vec<Subscriber<api::component::emergency_stop::State>>,
    safety_constraints: Subscriber<api::safety::MotionConstraints>,
    drive: Publisher<api::drive::Target>,
    state: Publisher<api::motion::State>,
}

#[phoxal::service(id = "motion", config = ())]
struct Motion {
    limits: MotionLimits,
    latest_manual: Arc<Mutex<Option<ManualCandidate>>>,
    last_autonomous: Option<Timed<api::navigation::Candidate>>,
    estop: EmergencyStopLatch,
    last_safety_constraints: Option<Timed<api::safety::MotionConstraints>>,
}

#[phoxal::behavior]
impl Motion {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let cap = ctx.owner_capability();
        let robot = ctx.robot()?;
        let limits = robot.manifest.robot.motion_limits.validate()?;
        let estop_bindings = emergency_stop_bindings(robot);
        let manual_subscriber = ctx
            .subscriber(api::topic::internal::new(cap).motion().manual(), 32)
            .await?;
        let latest_manual = Arc::new(Mutex::new(None));
        let manual_slot = Arc::clone(&latest_manual);
        let manual_receiver = manual_subscriber.clone();
        ctx.spawn_managed("manual-receiver", async move {
            receive_manual_forever(manual_receiver, manual_slot).await;
        });

        let mut component_estops = Vec::with_capacity(estop_bindings.len());
        for binding in &estop_bindings {
            component_estops.push(
                ctx.subscriber(
                    api::topic::new()
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
                latest_manual,
                last_autonomous: None,
                estop: EmergencyStopLatch::new(estop_bindings.len()),
                last_safety_constraints: None,
            },
            Self::Api {
                manual: manual_subscriber,
                autonomous: ctx
                    .subscriber(api::topic::new().navigation().candidate(), 32)
                    .await?,
                software_estop: ctx
                    .subscriber(api::topic::internal::new(cap).motion().estop(), 32)
                    .await?,
                component_estops,
                safety_constraints: ctx
                    .subscriber(api::topic::new().safety().constraints(), 32)
                    .await?,
                drive: ctx.publisher(api::topic::new().drive().target()).await?,
                state: ctx
                    .publisher(api::topic::internal::new(cap).motion().state())
                    .await?,
            },
        ))
    }

    #[step(hz = 20)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        let manual = latest_manual(&self.latest_manual);
        let host_now = Instant::now();
        while let Some(received) = api.autonomous.try_recv() {
            self.last_autonomous = Some(Timed {
                body: received.body,
                at: LogicalTime::new(received.metadata.epoch, received.metadata.produced_at_ns),
            });
        }
        while let Some(received) = api.software_estop.try_recv() {
            self.estop.set_software(received.body.engaged);
        }
        for (index, subscriber) in api.component_estops.iter().enumerate() {
            while let Some(received) = subscriber.try_recv() {
                self.estop.set_component(
                    index,
                    received.body.engaged,
                    LogicalTime::new(received.metadata.epoch, received.metadata.produced_at_ns),
                );
            }
        }
        while let Some(received) = api.safety_constraints.try_recv() {
            self.last_safety_constraints = Some(Timed {
                body: received.body,
                at: LogicalTime::new(received.metadata.epoch, received.metadata.produced_at_ns),
            });
        }

        let safety_runtime = if self
            .last_safety_constraints
            .as_ref()
            .is_some_and(|constraints| safety_is_usable(constraints, step.time()))
        {
            api::motion::SafetyRuntime::Present
        } else {
            api::motion::SafetyRuntime::Absent
        };
        let arbitration = arbitrate(
            manual.as_ref(),
            self.last_autonomous.as_ref(),
            self.estop.engaged(step.time()),
            self.last_safety_constraints.as_ref(),
            self.limits,
            step.time(),
            host_now,
        );
        self.estop.finish_cycle();

        api.drive
            .publish_at(step.time(), arbitration.selected.clone())
            .await?;
        api.state
            .publish_at(
                step.time(),
                api::motion::State {
                    manual_candidate_age_ns: manual_candidate_age_ns(manual.as_ref(), host_now),
                    autonomous_candidate_age_ns: candidate_age_ns(
                        self.last_autonomous.as_ref(),
                        step.time(),
                    ),
                    safety_constraints_age_ns: candidate_age_ns(
                        self.last_safety_constraints.as_ref(),
                        step.time(),
                    ),
                    selected_source: arbitration.source,
                    final_target: state_target(&arbitration.selected),
                    zero_reason: arbitration.zero_reason,
                    safety_runtime,
                    software_estop_engaged: self.estop.software_blocked(),
                    component_estop_blocked: self.estop.components_blocked(step.time()),
                    active_safety_constraints: self.last_safety_constraints.as_ref().map_or_else(
                        Vec::new,
                        |constraints| {
                            if safety_is_usable(constraints, step.time()) {
                                constraints.body.constraints.clone()
                            } else {
                                Vec::new()
                            }
                        },
                    ),
                },
            )
            .await?;
        Ok(())
    }
}

async fn receive_manual_forever(
    subscriber: Subscriber<api::motion::ManualCommand>,
    latest: Arc<Mutex<Option<ManualCandidate>>>,
) {
    loop {
        match subscriber.recv().await {
            Ok(received) => store_manual(&latest, received.body, Instant::now()),
            Err(_) => return,
        }
    }
}

fn store_manual(
    latest: &Mutex<Option<ManualCandidate>>,
    body: api::motion::ManualCommand,
    received_at: Instant,
) {
    *latest
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some(ManualCandidate { body, received_at });
}

fn latest_manual(latest: &Mutex<Option<ManualCandidate>>) -> Option<ManualCandidate> {
    latest
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
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

fn main() -> phoxal::Result<()> {
    phoxal::run::<Motion>()
}

#[cfg(test)]
mod tests {
    use phoxal::bus::ContractBody;
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};

    use super::*;

    #[test]
    fn api_owns_estop_and_state_and_consumes_component_estops() {
        assert_eq!(<Motion as Participant>::ID, "motion");
        let contracts = <<Motion as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert_contract::<api::motion::ManualCommand>(contracts, ContractRole::Subscribe);
        assert_contract::<api::navigation::Candidate>(contracts, ContractRole::Subscribe);
        assert_contract::<api::motion::EmergencyStopRequest>(contracts, ContractRole::Subscribe);
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

    #[test]
    fn manual_receiver_slot_is_bounded_and_newest_wins() {
        let latest = Mutex::new(None);
        let first_at = Instant::now();
        store_manual(
            &latest,
            api::motion::ManualCommand {
                linear_x_mps: 0.1,
                angular_z_radps: 0.2,
            },
            first_at,
        );
        let second_at = first_at + std::time::Duration::from_millis(10);
        store_manual(
            &latest,
            api::motion::ManualCommand {
                linear_x_mps: 0.3,
                angular_z_radps: 0.4,
            },
            second_at,
        );

        let manual = latest_manual(&latest).expect("latest command should be retained");
        assert_eq!(manual.body.linear_x_mps, 0.3);
        assert_eq!(manual.body.angular_z_radps, 0.4);
        assert_eq!(manual.received_at, second_at);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn manual_receiver_runs_without_logical_steps_and_keeps_newest() {
        let namespace = format!(
            "test/motion-manual-receiver/{}-{}",
            std::process::id(),
            phoxal::raw::host_time().time_ns()
        );
        let bus = phoxal::raw::Bus::open(phoxal::raw::BusConfig::in_process(namespace, "robot"))
            .await
            .expect("bus should open");
        let publisher =
            phoxal::raw::Publisher::new(bus.clone(), &api::topic::new().motion().manual())
                .expect("manual publisher should attach");
        let subscriber = Subscriber::new(
            &bus,
            &api::topic::internal::new(phoxal::raw::OwnerCap::__mint())
                .motion()
                .manual(),
            32,
        )
        .await
        .expect("manual subscriber should attach");
        let latest = Arc::new(Mutex::new(None));
        let receiver_slot = Arc::clone(&latest);
        let receiver = tokio::spawn(async move {
            receive_manual_forever(subscriber, receiver_slot).await;
        });

        for linear_x_mps in [0.1, 0.2, 0.3] {
            publisher
                .publish_at(
                    phoxal::raw::host_time(),
                    api::motion::ManualCommand {
                        linear_x_mps,
                        angular_z_radps: 0.0,
                    },
                )
                .await
                .expect("manual command should publish");
        }

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if latest_manual(&latest).is_some_and(|manual| manual.body.linear_x_mps == 0.3) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("receiver should advance independently of logical steps");

        let manual = latest_manual(&latest).expect("latest manual command should be retained");
        assert_eq!(manual.body.linear_x_mps, 0.3);
        receiver.abort();
        bus.close().await.expect("bus should close");
    }

    #[test]
    fn software_and_component_estops_latch_independently() {
        let mut latch = EmergencyStopLatch::new(2);
        let now = LogicalTime::new(0, 100);
        assert!(
            latch.engaged(now),
            "configured component e-stops must publish before motion"
        );
        latch.set_component(0, false, now);
        latch.set_component(1, false, now);
        latch.finish_cycle();
        assert!(!latch.engaged(now));

        latch.set_software(true);
        assert!(latch.engaged(now));
        latch.set_component(1, true, now);
        latch.finish_cycle();
        latch.set_software(false);
        assert!(
            latch.engaged(now),
            "software reset must not clear a component stop"
        );

        latch.set_component(1, false, now);
        latch.finish_cycle();
        assert!(!latch.engaged(now));
    }

    #[test]
    fn releasing_one_component_does_not_clear_another() {
        let mut latch = EmergencyStopLatch::new(2);
        let now = LogicalTime::new(0, 100);
        latch.set_software(false);
        latch.set_component(0, true, now);
        latch.set_component(1, true, now);
        latch.finish_cycle();
        latch.set_component(0, false, now);
        assert!(latch.engaged(now));
        latch.set_component(1, false, now);
        latch.finish_cycle();
        assert!(!latch.engaged(now));
    }

    #[test]
    fn engage_then_release_in_one_cycle_still_forces_a_stop_cycle() {
        let mut latch = EmergencyStopLatch::new(1);
        let now = LogicalTime::new(0, 100);
        latch.set_component(0, false, now);
        latch.set_software(true);
        latch.set_software(false);
        assert!(latch.engaged(now));
        latch.finish_cycle();
        assert!(!latch.engaged(now));
    }

    #[test]
    fn a_robot_without_component_estops_starts_ready_for_manual_control() {
        let latch = EmergencyStopLatch::new(0);
        assert!(!latch.engaged(LogicalTime::new(0, 100)));
    }

    #[test]
    fn missing_stale_future_and_prior_epoch_component_estops_fail_closed() {
        let mut latch = EmergencyStopLatch::new(1);
        latch.set_software(false);
        let now = LogicalTime::new(2, COMPONENT_ESTOP_STALE_NS + 10);
        assert!(latch.components_blocked(now));

        latch.set_component(0, false, LogicalTime::new(2, 9));
        assert!(latch.components_blocked(now));
        latch.set_component(0, false, LogicalTime::new(2, now.time_ns() + 1));
        assert!(latch.components_blocked(now));
        latch.set_component(0, false, LogicalTime::new(1, now.time_ns()));
        assert!(latch.components_blocked(now));
        latch.set_component(0, false, now);
        latch.finish_cycle();
        assert!(!latch.components_blocked(now));
    }
}
