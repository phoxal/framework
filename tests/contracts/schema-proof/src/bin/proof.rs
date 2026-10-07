//! Runs the fixture so its schema frames are reachable from a linked binary.

use phoxal_schema_proof_fixture::{Command, Control, Mode, Reason, Target, Telemetry};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let command = Command {
        mode: Mode::Manual,
        owner: Some("proof".to_owned()),
        targets: vec![Target {
            actuator_id: "wheel".to_owned(),
            control: Some(Control::VelocityRadps(1.5)),
        }],
        payload: vec![1, 2, 3],
        count: 7,
        depth_mm: vec![10, 20, 30],
        encoder: Some(phoxal::contracts::component::encoder::EncoderSample {
            position_rad: Some(0.25),
            velocity_radps: None,
        }),
        reasons: vec![Reason::Unavailable],
    };
    let telemetry = Telemetry {
        pose: None,
        twist: None,
        battery: None,
        scan: None,
    };
    let motion = phoxal::contracts::robotics::MotionSetpoint {
        linear_x_mps: 0.75,
        angular_z_radps: -0.5,
    };
    // This executable intentionally exports these roots without a runtime
    // contract; the SDK retains their full referenced closures from here.
    use phoxal::schema::MessageSchema as _;
    std::hint::black_box(Command::retain_schema());
    std::hint::black_box(Telemetry::retain_schema());
    std::hint::black_box(phoxal::contracts::robotics::MotionSetpoint::retain_schema());
    std::hint::black_box(phoxal::contracts::component::range::RangeSample::retain_schema());
    let protocols = phoxal::communication::file_descriptor_set()?;
    use prost::Message as _;
    println!(
        "{}",
        command.encode_to_vec().len()
            + telemetry.encode_to_vec().len()
            + motion.encode_to_vec().len()
            + protocols.encode_to_vec().len()
    );
    Ok(())
}
