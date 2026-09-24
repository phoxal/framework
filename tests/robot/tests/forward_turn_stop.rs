//! Finite native movement proof for the internal four-wheel robot.

use phoxal::scenario::{CapturePolicy, Simulation};
phoxal::api!();

use api::__contracts::phoxal::motion::v1::MotionIntent;
use api::controller;

const WHEELS: [&str; 4] = [
    "front_left_drive",
    "front_right_drive",
    "rear_left_drive",
    "rear_right_drive",
];

#[phoxal::scenario]
fn forward_turn_stop(sim: &mut Simulation) -> phoxal::Result<()> {
    let mut plan = sim.plan();
    let body = plan.record_body("phoxal-test-robot")?;
    let wheels = WHEELS
        .iter()
        .map(|wheel| {
            plan.record(
                api::__contracts::phoxal::component::ddsm115::v1::ddsm115::methods::ENCODER
                    .bind(wheel),
                CapturePolicy::best_effort_history(1_024)?,
            )
        })
        .collect::<phoxal::Result<Vec<_>>>()?;

    plan.send(manual(0.0, 0.0))?;
    plan.wait_steps(30)?;
    plan.send(manual(0.5, 0.0))?;
    plan.wait_steps(150)?;
    plan.send(manual(0.0, 2.0))?;
    plan.wait_steps(320)?;
    plan.send(manual(0.0, 0.0))?;
    plan.wait_steps(70)?;
    plan.send(controller::withdraw_manual())?;
    plan.wait_steps(30)?;

    let observed = sim.run(plan)?;
    for (wheel, capture) in WHEELS.into_iter().zip(wheels) {
        let moved = observed.history(&capture)?.iter().any(|sample| {
            sample
                .value()
                .velocity_radps
                .is_some_and(|value| value.abs() > 0.1)
        });
        assert!(moved, "{wheel} never observed wheel motion");
    }

    let samples = observed.body_history(&body)?;
    let first = samples
        .first()
        .ok_or_else(|| phoxal::anyhow!("native body history is empty"))?
        .value();
    let last = samples
        .last()
        .ok_or_else(|| phoxal::anyhow!("native body history is empty"))?
        .value();
    let displacement = ((last.position_m[0] - first.position_m[0]).powi(2)
        + (last.position_m[1] - first.position_m[1]).powi(2))
    .sqrt();
    assert!(
        displacement >= 0.5,
        "displacement {displacement:.3} m is below 0.5 m"
    );
    let yaw_change = samples.windows(2).fold(0.0, |total, pair| {
        let mut delta =
            yaw(pair[1].value().orientation_wxyz) - yaw(pair[0].value().orientation_wxyz);
        if delta > std::f64::consts::PI {
            delta -= std::f64::consts::TAU;
        } else if delta < -std::f64::consts::PI {
            delta += std::f64::consts::TAU;
        }
        total + delta
    });
    assert!(
        yaw_change.abs() >= 1.0,
        "yaw change {yaw_change:.3} rad is below 1.0 rad"
    );
    let final_linear_speed = last
        .linear_velocity_mps
        .iter()
        .map(|value| value.powi(2))
        .sum::<f64>()
        .sqrt();
    assert!(
        final_linear_speed < 0.03 && last.angular_velocity_radps[2].abs() < 0.05,
        "robot did not stop (linear {final_linear_speed:.3} m/s, yaw {:.3} rad/s)",
        last.angular_velocity_radps[2]
    );
    Ok(())
}

fn manual(
    linear_x_mps: f64,
    angular_z_radps: f64,
) -> impl phoxal::scenario::SendOperation<Response = phoxal::contract::Empty> {
    controller::manual(MotionIntent {
        linear_x_mps,
        angular_z_radps,
    })
}

fn yaw(q: [f64; 4]) -> f64 {
    let [w, x, y, z] = q;
    (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z))
}
