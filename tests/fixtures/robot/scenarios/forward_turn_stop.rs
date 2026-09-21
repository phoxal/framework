//! Finite native movement proof for the internal four-wheel robot.

use std::time::Duration;

use phoxal::robotics::EncoderSample;
use phoxal::scenario::{Action, Capture, CaptureRecord, Scenario, ScenarioPlan, Step, Validity};
use prost::Message;

const DURATION_MICROS: u64 = 6_000_000;
const WHEELS: [&str; 4] = [
    "front_left_drive",
    "front_right_drive",
    "rear_left_drive",
    "rear_right_drive",
];

#[derive(Debug, Default)]
pub struct ForwardTurnStop;

#[phoxal::scenario]
impl Scenario for ForwardTurnStop {
    fn plan(&self) -> phoxal::Result<ScenarioPlan> {
        let steps = vec![
            intent_step("initial-stop", 0, 0.0, 0.0)?,
            intent_step("forward", 30, 0.5, 0.0)?,
            intent_step("turn", 180, 0.0, 2.0)?,
            intent_step("stop", 500, 0.0, 0.0)?,
            Step::new(
                "withdraw",
                570,
                Action::withdraw("controller", motion::ports::MANUAL.signature())
                    .map_err(|error| phoxal::anyhow!("withdraw action: {error}"))?,
            ),
        ];

        let mut captures = WHEELS
            .iter()
            .map(|wheel| {
                Capture::sample(
                    format!("{wheel}/encoder"),
                    ddsm115::ports::ENCODER.signature(),
                )
                .map_err(|error| phoxal::anyhow!("encoder capture: {error}"))
            })
            .collect::<phoxal::Result<Vec<_>>>()?;
        captures.push(
            Capture::native_body("phoxal-test-robot", "SI", "world")
                .map_err(|error| phoxal::anyhow!("native body capture: {error}"))?,
        );

        ScenarioPlan::with_steps(
            "simulation/scene.xml",
            Duration::from_micros(DURATION_MICROS),
            steps,
            captures,
        )
        .map_err(|error| phoxal::anyhow!("plan validate: {error}"))
    }

    fn verify(&self, run: &phoxal::scenario::ScenarioRun) -> phoxal::Result<()> {
        if !run.is_sealed() || !run.passed() {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: lifecycle did not seal a passing run"
            ));
        }
        for boundary in [
            "s00000000",
            "s00000001",
            "s00000002",
            "s00000003",
            "s00000004",
        ] {
            run.outcome(boundary).ok_or_else(|| {
                phoxal::anyhow!("ForwardTurnStop: step {boundary} has no outcome")
            })?;
        }

        for wheel in WHEELS {
            let name = format!("{wheel}/encoder");
            let CaptureRecord::Samples(samples) = run
                .capture(&name)
                .ok_or_else(|| phoxal::anyhow!("ForwardTurnStop: {name} capture missing"))?
            else {
                return Err(phoxal::anyhow!(
                    "ForwardTurnStop: {name} is not sample evidence"
                ));
            };
            let observed_motion =
                samples
                    .iter()
                    .try_fold(false, |observed, bytes| -> phoxal::Result<bool> {
                        let sample = EncoderSample::decode(bytes.as_slice()).map_err(|error| {
                            phoxal::anyhow!("ForwardTurnStop: invalid {name} protobuf: {error}")
                        })?;
                        sample.validate().map_err(|error| {
                            phoxal::anyhow!("ForwardTurnStop: invalid {name} sample: {error}")
                        })?;
                        Ok(
                            observed
                                || sample.velocity_radps.is_some_and(|value| value.abs() > 0.1),
                        )
                    })?;
            if !observed_motion {
                return Err(phoxal::anyhow!(
                    "ForwardTurnStop: {name} never observed wheel motion"
                ));
            }
        }

        let CaptureRecord::NativeBody(bytes) = run
            .capture("phoxal-test-robot")
            .ok_or_else(|| phoxal::anyhow!("ForwardTurnStop: native body capture missing"))?
        else {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: robot capture is not native body evidence"
            ));
        };
        let samples: Vec<phoxal::scenario::NativeBodySample> = serde_json::from_slice(bytes)
            .map_err(|error| phoxal::anyhow!("ForwardTurnStop: invalid body evidence: {error}"))?;
        let first = samples
            .first()
            .ok_or_else(|| phoxal::anyhow!("ForwardTurnStop: body history is empty"))?;
        let last = samples
            .last()
            .ok_or_else(|| phoxal::anyhow!("ForwardTurnStop: body history is empty"))?;
        let displacement = ((last.position_m[0] - first.position_m[0]).powi(2)
            + (last.position_m[1] - first.position_m[1]).powi(2))
        .sqrt();
        if displacement < 0.5 {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: displacement {displacement:.3} m is below 0.5 m"
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
                "ForwardTurnStop: yaw change {yaw_change:.3} rad is below 1.0 rad"
            ));
        }
        let final_linear_speed = last
            .linear_velocity_mps
            .iter()
            .map(|value| value.powi(2))
            .sum::<f64>()
            .sqrt();
        if final_linear_speed >= 0.03 || last.angular_velocity_radps[2].abs() >= 0.05 {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: robot did not stop (linear {final_linear_speed:.3} m/s, yaw {:.3} rad/s)",
                last.angular_velocity_radps[2]
            ));
        }

        let evidence = run
            .terminal_evidence()
            .ok_or_else(|| phoxal::anyhow!("ForwardTurnStop: terminal evidence missing"))?;
        if evidence.quantum_ns() == 0
            || evidence.completed_transitions() == 0
            || !evidence.final_observation_cut()
            || !evidence.final_capture_drain()
            || !evidence.cleanup_ok()
        {
            return Err(phoxal::anyhow!(
                "ForwardTurnStop: terminal lifecycle evidence is incomplete"
            ));
        }
        Ok(())
    }
}

fn intent_step(
    label: &'static str,
    boundary: u32,
    linear_x_mps: f64,
    angular_z_radps: f64,
) -> phoxal::Result<Step> {
    let payload = motion::MotionIntent {
        owner_id: "scenarios/ForwardTurnStop".to_owned(),
        linear_x_mps,
        angular_z_radps,
    }
    .encode_to_vec();
    Ok(Step::new(
        label,
        boundary,
        Action::setpoint(
            "controller",
            motion::ports::MANUAL.signature(),
            payload,
            Validity::Permanent,
        )
        .map_err(|error| phoxal::anyhow!("{label} action: {error}"))?,
    ))
}

fn yaw(q: [f64; 4]) -> f64 {
    let [w, x, y, z] = q;
    (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z))
}
