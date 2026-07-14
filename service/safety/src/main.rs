//! `safety` - the official battery + drive safety monitor.
//!
//! This official participant MIXES two API versions (D1's
//! per-contract-versioning ground-breaker): it consumes required
//! `v2/battery/state` (the moved contract) and `v1/drive/state`
//! inputs plus emergency-stop sources (the software `safety/estop` request and
//! per-component emergency-stop states, both `v1`), and publishes the
//! robot's aggregate `safety/state` posture plus the current
//! `safety/authorization` motion envelope (also `v1`). There is no
//! per-participant API version ceiling - a participant's `Api` struct declares
//! whichever version-qualified contract each field needs.
//!
//! The monitor is fail-closed. A missing or stale required source forces `Stop`,
//! and any engaged emergency stop forces `EmergencyStop`. Otherwise it takes the
//! worst decision across battery charge (`Slow` when low, `Stop` when critical)
//! and drive stop-reason (`Stop` on fault, `EmergencyStop` on drive e-stop). The
//! authorization carries the approved-motion constraint for that decision
//! (`Stop`/`EmergencyStop` authorize zero motion) and a short TTL, so a stalled
//! `safety` participant lets the authorization expire downstream rather than leaving
//! a stale envelope in force. Battery is required only when the robot model
//! declares a battery capability; `drive/state` is always required.

mod assessment;
mod robot_config;

use phoxal::prelude::*;
use phoxal_api::v1 as api;
use phoxal_api::v2;

use crate::assessment::{SafetyInputs, Timed, assess, authorize, emergency_stop_engaged};
use crate::robot_config::{RequiredSources, emergency_stop_bindings, required_sources};

// `Api` deliberately mixes two contract versions: `battery` targets the
// standalone `v2` version (moved out of `v1`, D1's
// per-contract-versioning ground-breaker), while every other field stays on
// `v1`. A participant has no per-participant API version ceiling
// (`phoxal::participant::api`'s module docs) - this is the real, end-to-end
// proof of that.
#[derive(phoxal::Api)]
struct Api {
    battery: Subscriber<v2::battery::State>,
    drive: Subscriber<api::drive::State>,
    software_estop: Subscriber<api::safety::EmergencyStopRequest>,
    component_estops: Vec<Subscriber<api::component::emergency_stop::State>>,
    authorization: Publisher<api::safety::SafetyAuthorization>,
    state: Publisher<api::safety::Status>,
}

#[phoxal::service(id = "safety", config = ())]
struct Safety {
    // Runtime-private typed state (not handles).
    required: RequiredSources,
    last_battery: Option<Timed<v2::battery::State>>,
    last_drive: Option<Timed<api::drive::State>>,
    software_estop_engaged: bool,
    component_estop_engaged: Vec<bool>,
}

#[phoxal::behavior]
impl Safety {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        let robot = ctx.robot()?;
        let required = required_sources(robot);
        let emergency_stop_bindings = emergency_stop_bindings(robot);

        let battery = ctx
            .subscriber(v2::topic::new().battery().state(), 32)
            .await?;
        let drive = ctx
            .subscriber(api::topic::new().drive().state(), 32)
            .await?;
        // Safety OWNS the `safety` node: it reads its e-stop command input here, and
        // publishes `authorization`/`state` below, all through the owner
        // (`internal`) builder. `battery/state`, `drive/state` and the component
        // e-stop states are CONSUMED via the public builder.
        let software_estop = ctx
            .subscriber(api::topic::internal::new(cap).safety().estop(), 32)
            .await?;
        let mut component_estops = Vec::with_capacity(emergency_stop_bindings.len());
        for binding in &emergency_stop_bindings {
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
        let authorization = ctx
            .publisher(api::topic::internal::new(cap).safety().authorization())
            .await?;
        let state = ctx
            .publisher(api::topic::internal::new(cap).safety().state())
            .await?;

        Ok((
            Self {
                required,
                last_battery: None,
                last_drive: None,
                software_estop_engaged: false,
                component_estop_engaged: vec![false; emergency_stop_bindings.len()],
            },
            Self::Api {
                battery,
                drive,
                software_estop,
                component_estops,
                authorization,
                state,
            },
        ))
    }

    #[step(hz = 10)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        while let Some(received) = api.battery.try_recv() {
            self.last_battery = Some(Timed {
                body: received.body,
                produced_at_ns: received.metadata.produced_at_ns,
            });
        }
        while let Some(received) = api.drive.try_recv() {
            self.last_drive = Some(Timed {
                body: received.body,
                produced_at_ns: received.metadata.produced_at_ns,
            });
        }
        while let Some(received) = api.software_estop.try_recv() {
            self.software_estop_engaged = received.body.engaged;
        }
        for (index, subscriber) in api.component_estops.iter_mut().enumerate() {
            while let Some(received) = subscriber.try_recv() {
                self.component_estop_engaged[index] = received.body.engaged;
            }
        }

        let now_ns = step.time().time_ns();
        let inputs = SafetyInputs {
            required: self.required,
            battery: self.last_battery.as_ref(),
            drive: self.last_drive.as_ref(),
            emergency_stop_engaged: emergency_stop_engaged(
                self.software_estop_engaged,
                &self.component_estop_engaged,
            ),
        };
        let status = assess(&inputs, now_ns);
        let authorization = authorize(&status, &inputs, now_ns);
        api.authorization
            .publish_at(step.time(), authorization)
            .await?;
        api.state.publish_at(step.time(), status).await?;
        Ok(())
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Safety>()
}

#[cfg(test)]
mod tests {
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};
    use phoxal_api::ContractBody;

    use super::{RequiredSources, Safety, SafetyInputs, Timed, api, assess, authorize, v2};
    use crate::assessment::SOURCE_FRESH_NS;

    const NOW_NS: u64 = 1_000_000_000;
    const REQUIRED: RequiredSources = RequiredSources {
        battery: true,
        drive: true,
    };

    #[test]
    fn nominal_when_all_healthy() {
        let battery = timed(NOW_NS, battery(0.9));
        let drive = timed(NOW_NS, drive(None));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::Allow);
        assert!(status.active_reasons.is_empty());
    }

    #[test]
    fn battery_low_slows() {
        let battery = timed(NOW_NS, battery(0.2));
        let drive = timed(NOW_NS, drive(None));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::Slow);
        assert_eq!(
            status.active_reasons[0].code,
            api::safety::SafetyReasonCode::BatteryLow
        );
    }

    #[test]
    fn battery_critical_stops() {
        let battery = timed(NOW_NS, battery(0.05));
        let drive = timed(NOW_NS, drive(None));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::Stop);
        assert_eq!(
            status.active_reasons[0].code,
            api::safety::SafetyReasonCode::BatteryCritical
        );
    }

    #[test]
    fn drive_fault_stops() {
        let battery = timed(NOW_NS, battery(0.9));
        let drive = timed(NOW_NS, drive(Some(api::drive::StopReason::Fault)));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::Stop);
        assert!(
            status
                .active_reasons
                .iter()
                .any(|r| r.code == api::safety::SafetyReasonCode::DriveFault)
        );
    }

    #[test]
    fn drive_emergency_stop_engages_emergency_stop() {
        let battery = timed(NOW_NS, battery(0.9));
        let drive = timed(NOW_NS, drive(Some(api::drive::StopReason::EmergencyStop)));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::EmergencyStop);
        assert!(
            status
                .active_reasons
                .iter()
                .any(|r| r.code == api::safety::SafetyReasonCode::EmergencyStopEngaged)
        );
        assert_zero_motion(&authorize(&status, &inputs, NOW_NS).approved_motion);
    }

    #[test]
    fn worst_decision_wins() {
        let battery = timed(NOW_NS, battery(0.2));
        let drive = timed(NOW_NS, drive(Some(api::drive::StopReason::Fault)));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::Stop);
        assert!(
            status
                .active_reasons
                .iter()
                .any(|r| r.code == api::safety::SafetyReasonCode::BatteryLow)
        );
        assert!(
            status
                .active_reasons
                .iter()
                .any(|r| r.code == api::safety::SafetyReasonCode::DriveFault)
        );
    }

    #[test]
    fn emergency_stop_input_wins_and_authorizes_zero() {
        let battery = timed(NOW_NS, battery(0.2));
        let drive = timed(NOW_NS, drive(Some(api::drive::StopReason::Fault)));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), true);
        let status = assess(&inputs, NOW_NS);
        let authorization = authorize(&status, &inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::EmergencyStop);
        assert_eq!(status.active_reasons.len(), 1);
        assert_eq!(
            status.active_reasons[0].code,
            api::safety::SafetyReasonCode::EmergencyStopEngaged
        );
        assert_eq!(
            authorization.decision,
            api::safety::SafetyDecision::EmergencyStop
        );
        assert_zero_motion(&authorization.approved_motion);
    }

    #[test]
    fn emergency_stop_latch_composes_software_and_component_sources() {
        assert!(super::emergency_stop_engaged(true, &[false, false]));
        assert!(super::emergency_stop_engaged(false, &[false, true]));
        assert!(!super::emergency_stop_engaged(false, &[false, false]));
    }

    #[test]
    fn missing_required_inputs_fail_closed() {
        let inputs = inputs(REQUIRED, None, None, false);
        let status = assess(&inputs, NOW_NS);
        let authorization = authorize(&status, &inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::Stop);
        assert!(
            status
                .active_reasons
                .iter()
                .any(|r| r.code == api::safety::SafetyReasonCode::SourceStale)
        );
        assert_zero_motion(&authorization.approved_motion);
    }

    #[test]
    fn stale_required_source_fails_closed() {
        let now_ns = SOURCE_FRESH_NS + 1;
        let battery = timed(0, battery(0.9));
        let drive = timed(now_ns, drive(None));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, now_ns);
        let authorization = authorize(&status, &inputs, now_ns);

        assert_eq!(status.decision, api::safety::SafetyDecision::Stop);
        assert!(
            status
                .active_reasons
                .iter()
                .any(|r| r.code == api::safety::SafetyReasonCode::SourceStale)
        );
        assert_zero_motion(&authorization.approved_motion);
    }

    #[test]
    fn optional_missing_battery_does_not_block_nominal_drive() {
        let drive = timed(NOW_NS, drive(None));
        let inputs = inputs(
            RequiredSources {
                battery: false,
                drive: true,
            },
            None,
            Some(&drive),
            false,
        );
        let status = assess(&inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::Allow);
        assert!(status.active_reasons.is_empty());
    }

    #[test]
    fn authorization_matches_status() {
        let battery = timed(1_000, battery(0.2));
        let drive = timed(1_000, drive(None));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, 1_000);
        let authorization = authorize(&status, &inputs, 1_000);

        assert_eq!(authorization.decision, api::safety::SafetyDecision::Slow);
        assert_eq!(authorization.expires_at_ns, Some(100_001_000));
        assert_eq!(authorization.reasons, status.active_reasons);
        assert_eq!(authorization.approved_motion.linear_x_mps.max, 0.1);
    }

    #[test]
    fn api_reports_mixed_version_contracts() {
        assert_eq!(<Safety as Participant>::ID, "safety");

        // The moved contract's version-qualified wire key (D1): battery
        // subscribes on v2, everything else stays on v1.
        assert_eq!(
            <v2::battery::State as ContractBody>::TOPIC,
            "v2/battery/state"
        );

        let contracts = <<Safety as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert_contract::<v2::battery::State>(contracts, ContractRole::Subscribe);
        assert_contract::<api::drive::State>(contracts, ContractRole::Subscribe);
        assert_contract::<api::safety::EmergencyStopRequest>(contracts, ContractRole::Subscribe);
        assert_contract::<api::component::emergency_stop::State>(
            contracts,
            ContractRole::Subscribe,
        );
        assert_contract::<api::safety::SafetyAuthorization>(contracts, ContractRole::Publish);
        assert_contract::<api::safety::Status>(contracts, ContractRole::Publish);
    }

    fn assert_contract<B>(contracts: &[phoxal::participant::ApiContractUse], role: ContractRole)
    where
        B: ContractBody,
    {
        assert!(
            contracts
                .iter()
                .any(|c| c.topic == B::TOPIC && c.role == role),
            "expected a {role:?} contract for {} in {contracts:?}",
            B::TOPIC
        );
    }

    fn inputs<'a>(
        required: RequiredSources,
        battery: Option<&'a Timed<v2::battery::State>>,
        drive: Option<&'a Timed<api::drive::State>>,
        emergency_stop_engaged: bool,
    ) -> SafetyInputs<'a> {
        SafetyInputs {
            required,
            battery,
            drive,
            emergency_stop_engaged,
        }
    }

    fn timed<T>(produced_at_ns: u64, body: T) -> Timed<T> {
        Timed {
            body,
            produced_at_ns,
        }
    }

    fn battery(charge_ratio: f32) -> v2::battery::State {
        v2::battery::State {
            voltage_v: 15.0,
            current_a: 1.0,
            charge_ratio,
        }
    }

    fn drive(stop_reason: Option<api::drive::StopReason>) -> api::drive::State {
        let target = api::drive::Target {
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
            curvature_limit_radpm: None,
        };

        api::drive::State {
            target: target.clone(),
            limited_target: target,
            actuator_authority: api::drive::ActuatorAuthority::Active,
            stop_reason,
        }
    }

    fn assert_zero_motion(approved_motion: &api::safety::MotionConstraint) {
        assert_eq!(approved_motion.linear_x_mps.min, 0.0);
        assert_eq!(approved_motion.linear_x_mps.max, 0.0);
        assert_eq!(approved_motion.angular_z_radps.min, 0.0);
        assert_eq!(approved_motion.angular_z_radps.max, 0.0);
    }
}
