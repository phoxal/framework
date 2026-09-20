//! ForwardTurnStop native movement scenario.
//!
//! Drives the rover through the canonical arm → forward → turn → stop
//! → disarm sequence over a finite controlled execution. The scenario
//! uses:
//!   - `simulation/scene.xml` (the rover's authored scene).
//!   - Real `phoxal-service-motion` types (`MotionIntent`,
//!     `ApplyEmergencyRequest`, `MotionStatus`) — no manual protobuf
//!     encoder and no handwritten descriptor strings.
//!   - `Motion/Manual` setpoint port for forward / turn / stop
//!     intents.
//!   - `Motion/Status` state capture for the controlled motion
//!     observation.
//!   - `Motion/Emergency` commands port for explicit arm / disarm.
//!   - `Safety/Status` state capture for the safety observation.
//!
//! The six-second program uses persistent intent followed by an explicit
//! zero setpoint, disarm command, and withdrawal.
//! The native quantum is 10 ms, so the program completes 600 controlled
//! transitions before verification.

use std::time::Duration;

use phoxal::scenario::{Action, Capture, Scenario, ScenarioPlan, Step, Validity};
use prost::Message;

use motion::{
    ApplyEmergencyRequest, ApplyEmergencyResponse, Arm, ControlMode, Disarm, MotionIntent,
};

/// Six seconds at the rover's 10 ms native quantum.
pub(crate) const DURATION_MICROS: u64 = 6_000_000;

/// Forward-turn-stop scenario that drives the rover's canonical
/// arm → forward → turn → stop → disarm sequence against
/// `simulation/scene.xml`. The rover owns the arm/disarm behavior;
/// the scenarios are observed-only beyond the boundary the user
/// authors.
#[derive(Debug, Default)]
pub struct ForwardTurnStop;

#[phoxal::scenario]
impl Scenario for ForwardTurnStop {
    fn plan(&self) -> phoxal::Result<ScenarioPlan> {
        let steps = vec![
            // Allow provider observations, kinematics, safety, and Motion to
            // establish a valid initial cut before requesting authority.
            Step::new(
                "initial-stop",
                0,
                Action::setpoint(
                    "motion",
                    motion::ports::MANUAL.signature(),
                    encode_motion_intent("scenarios/ForwardTurnStop", 0.0, 0.0),
                    Validity::Permanent,
                )
                .map_err(|e| phoxal::anyhow!("initial stop setpoint: {e}"))?,
            ),
            Step::new(
                "arm",
                20,
                Action::command(
                    "motion",
                    motion::ports::EMERGENCY.signature(),
                    encode_arm_request("scenarios/ForwardTurnStop"),
                    "arm-rover",
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                )
                .map_err(|e| phoxal::anyhow!("arm action: {e}"))?,
            ),
            Step::new(
                "forward",
                30,
                Action::setpoint(
                    "motion",
                    motion::ports::MANUAL.signature(),
                    encode_motion_intent("scenarios/ForwardTurnStop", 0.5, 0.0),
                    Validity::Permanent,
                )
                .map_err(|e| phoxal::anyhow!("forward setpoint: {e}"))?,
            ),
            Step::new(
                "turn",
                180,
                Action::setpoint(
                    "motion",
                    motion::ports::MANUAL.signature(),
                    encode_motion_intent("scenarios/ForwardTurnStop", 0.0, 2.2),
                    Validity::Permanent,
                )
                .map_err(|e| phoxal::anyhow!("turn setpoint: {e}"))?,
            ),
            Step::new(
                "stop",
                500,
                Action::setpoint(
                    "motion",
                    motion::ports::MANUAL.signature(),
                    encode_motion_intent("scenarios/ForwardTurnStop", 0.0, 0.0),
                    Validity::Permanent,
                )
                .map_err(|e| phoxal::anyhow!("stop setpoint: {e}"))?,
            ),
            Step::new(
                "disarm",
                560,
                Action::command(
                    "motion",
                    motion::ports::EMERGENCY.signature(),
                    encode_disarm_request(),
                    "disarm-rover",
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                )
                .map_err(|e| phoxal::anyhow!("disarm action: {e}"))?,
            ),
            Step::new(
                "withdraw-manual",
                570,
                Action::withdraw("motion", motion::ports::MANUAL.signature())
                    .map_err(|e| phoxal::anyhow!("manual withdraw: {e}"))?,
            ),
        ];

        let captures = vec![
            Capture::state("motion/status", motion::ports::STATUS.signature())
                .map_err(|e| phoxal::anyhow!("motion status capture: {e}"))?,
            Capture::state("safety/status", safety::ports::STATUS.signature())
                .map_err(|e| phoxal::anyhow!("safety status capture: {e}"))?,
            Capture::native_body("example-simulation-rover", "SI", "world")
                .map_err(|e| phoxal::anyhow!("native body capture: {e}"))?,
        ];

        ScenarioPlan::with_steps(
            "simulation/scene.xml",
            Duration::from_micros(DURATION_MICROS),
            steps,
            captures,
        )
        .map_err(|e| phoxal::anyhow!("plan validate: {e}"))
    }

    fn verify(&self, run: &phoxal::scenario::ScenarioRun) -> phoxal::Result<()> {
        if !run.is_sealed() {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: scenario run was not sealed; the case host must seal before verify"
            ));
        }
        if !run.passed() {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: scenario run did not pass; lifecycle terminal evidence rejected"
            ));
        }

        // Every step boundary must have a recorded outcome.
        for boundary in [
            "s00000000",
            "s00000001",
            "s00000002",
            "s00000003",
            "s00000004",
            "s00000005",
            "s00000006",
        ] {
            run.outcome(boundary).ok_or_else(|| {
                phoxal::anyhow!("ForwardTurnStop: step {boundary} has no recorded outcome")
            })?;
        }
        for label in ["arm-rover", "disarm-rover"] {
            let reply = run.command_reply(label).ok_or_else(|| {
                phoxal::anyhow!("ForwardTurnStop: command reply `{label}` missing")
            })?;
            let phoxal::scenario::CommandReply::Accepted { response_bytes } = reply else {
                return Err(phoxal::anyhow!(
                    "ForwardTurnStop: command `{label}` was not accepted"
                ));
            };
            let response =
                ApplyEmergencyResponse::decode(response_bytes.as_slice()).map_err(|error| {
                    phoxal::anyhow!(
                        "ForwardTurnStop: command `{label}` returned invalid protobuf: {error}"
                    )
                })?;
            if !matches!(
                response.decision,
                Some(motion::apply_emergency_response::Decision::Accepted(_))
            ) {
                return Err(phoxal::anyhow!(
                    "ForwardTurnStop: command `{label}` returned a refusal"
                ));
            }
        }

        // Motion and safety status captures must be observed.
        let motion_status = run
            .capture("motion/status")
            .ok_or_else(|| phoxal::anyhow!("ForwardTurnStop: motion/status capture missing"))?;
        let phoxal::scenario::CaptureRecord::State(motion_status) = motion_status else {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: motion/status is not state evidence"
            ));
        };
        let motion_status =
            motion::MotionStatus::decode(motion_status.as_slice()).map_err(|error| {
                phoxal::anyhow!("ForwardTurnStop: invalid motion status protobuf: {error}")
            })?;
        if motion_status.mode != ControlMode::Disarmed as i32 || !motion_status.stopped {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: final Motion status is not disarmed and stopped"
            ));
        }
        run.capture("safety/status")
            .ok_or_else(|| phoxal::anyhow!("ForwardTurnStop: safety/status capture missing"))?;
        let body = run
            .capture("example-simulation-rover")
            .ok_or_else(|| phoxal::anyhow!("ForwardTurnStop: native body capture missing"))?;
        let phoxal::scenario::CaptureRecord::NativeBody(bytes) = body else {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: native robot capture is not native body evidence"
            ));
        };
        let samples: Vec<phoxal::scenario::NativeBodySample> = serde_json::from_slice(bytes)
            .map_err(|error| {
                phoxal::anyhow!("ForwardTurnStop: invalid native body evidence: {error}")
            })?;
        let first = samples
            .first()
            .ok_or_else(|| phoxal::anyhow!("ForwardTurnStop: native body history is empty"))?;
        let last = samples
            .last()
            .ok_or_else(|| phoxal::anyhow!("ForwardTurnStop: native body history is empty"))?;
        let displacement = ((last.position_m[0] - first.position_m[0]).powi(2)
            + (last.position_m[1] - first.position_m[1]).powi(2))
        .sqrt();
        if displacement < 0.5 {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: rover displacement {displacement:.3} m is below 0.5 m"
            ));
        }
        let yaw_change = samples.windows(2).fold(0.0, |total, pair| {
            let mut delta = yaw(pair[1].orientation_wxyz) - yaw(pair[0].orientation_wxyz);
            if delta > std::f64::consts::PI {
                delta -= std::f64::consts::TAU;
            } else if delta < -std::f64::consts::PI {
                delta += std::f64::consts::TAU;
            }
            total + delta
        });
        if yaw_change.abs() < 1.0 {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: rover yaw change {yaw_change:.3} rad is below 1.0 rad"
            ));
        }
        let final_linear_speed = (last.linear_velocity_mps[0].powi(2)
            + last.linear_velocity_mps[1].powi(2)
            + last.linear_velocity_mps[2].powi(2))
        .sqrt();
        if final_linear_speed >= 0.03 || last.angular_velocity_radps[2].abs() >= 0.05 {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: rover did not stop (linear {final_linear_speed:.3} m/s, yaw {:.3} rad/s)",
                last.angular_velocity_radps[2]
            ));
        }

        // The terminal evidence must carry the lifecycle-observed
        // supervisor-run identity (the bundle's run_id, not a
        // locally fabricated string).
        let evidence = run
            .terminal_evidence()
            .ok_or_else(|| phoxal::anyhow!("ForwardTurnStop: terminal evidence missing"))?;
        if evidence.quantum_ns() == 0 {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: lifecycle-observed quantum is zero; the case host \
                 did not record the supervisor's terminal facts"
            ));
        }
        if evidence.completed_transitions() == 0 {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: lifecycle-observed completed transitions are zero; \
                 the simulator did not complete the controlled execution"
            ));
        }
        if !evidence.final_observation_cut() {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: lifecycle did not observe the final motion / safety cut"
            ));
        }
        if !evidence.final_capture_drain() {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: lifecycle did not drain the capture buffer"
            ));
        }
        if !evidence.cleanup_ok() {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: lifecycle cleanup did not succeed"
            ));
        }

        Ok(())
    }
}

fn encode_motion_intent(owner_id: &str, linear_x_mps: f64, angular_z_radps: f64) -> Vec<u8> {
    let intent = MotionIntent {
        owner_id: owner_id.to_owned(),
        linear_x_mps,
        angular_z_radps,
    };
    intent.encode_to_vec()
}

fn yaw([w, x, y, z]: [f64; 4]) -> f64 {
    (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z))
}

fn encode_arm_request(owner_id: &str) -> Vec<u8> {
    let request = ApplyEmergencyRequest {
        command: Some(motion::apply_emergency_request::Command::Arm(Arm {
            mode: ControlMode::Manual as i32,
            owner_id: owner_id.to_owned(),
        })),
    };
    request.encode_to_vec()
}

fn encode_disarm_request() -> Vec<u8> {
    let request = ApplyEmergencyRequest {
        command: Some(motion::apply_emergency_request::Command::Disarm(Disarm {})),
    };
    request.encode_to_vec()
}
