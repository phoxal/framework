//! Bundle boundary regression tests.
//!
//! These stay in one crate-root unit target because they exercise the complete
//! persisted-bundle boundary across document validation, staged writing,
//! layout validation, selection, and verified reading. Splitting the shared
//! staged-bundle fixture among the implementation modules would duplicate the
//! boundary it proves.

use std::collections::BTreeMap;

use crate::*;
use phoxal_model::{AssetId, Clock, Robot};
use phoxal_runtime_contract::identity::ParticipantId;
use phoxal_runtime_contract::metadata::ParticipantRequirement;

use phoxal_model::RobotBuilder;
use phoxal_model::builder::Kinematics;
use phoxal_model::component::capability::{Capability, Motor, MotorCommand, StructuralTarget};
use phoxal_model::identity::{ComponentInstanceId, JointId};
use phoxal_runtime_contract::metadata::{ParticipantContract, ParticipantKind};
use phoxal_runtime_contract::version::FrameworkVersion;

type StagedBytes = (
    RuntimeDocument,
    BTreeMap<AssetId, Vec<u8>>,
    BTreeMap<BundlePath, BinarySource>,
);

/// Write an executable source the writer will accept as a binary input.
fn write_executable(path: &std::path::Path, bytes: &[u8]) {
    std::fs::write(path, bytes).expect("executable source bytes");
    #[cfg(unix)]
    std::fs::set_permissions(
        path,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .expect("executable source mode");
}

fn document() -> StagedBytes {
    let robot = RobotBuilder::new("rover")
        .build()
        .expect("minimal robot is valid");
    let asset_id = AssetId::new("robot/structure.json").expect("asset id");
    let asset_bytes = b"compiled structure".to_vec();
    let mut assets = BTreeMap::new();
    assets.insert(asset_id, asset_bytes);
    let binary_path = BundlePath::new("bin/drive").expect("binary path");
    let binary_source = std::env::current_exe().expect("test binary path");
    let drive_source = BinarySource::open(&binary_source).expect("drive executable source");
    let mut binaries = BTreeMap::new();
    let artifact_id = ParticipantArtifactId::new("drive").expect("artifact id");
    let binary = BinaryReference::from_source(
        binary_path.clone(),
        ParticipantContract {
            framework: FrameworkVersion::CURRENT,
            id: artifact_id.clone(),
            kind: ParticipantKind::Service,
            requirement: None,
            config_schema: serde_json::json!({"type":"null"}),
        },
        &drive_source,
    )
    .expect("test binary hashes");
    let participant = RuntimeParticipant::new(
        ParticipantId::new("drive").expect("participant id"),
        artifact_id.clone(),
        None,
        None,
        ParticipantClock::Real,
    );
    let brain_id = ParticipantArtifactId::new("brain").expect("brain artifact id");
    let brain_path = BundlePath::new("bin/brain").expect("brain binary path");
    let brain_source = BinarySource::open(&binary_source).expect("brain executable source");
    let brain = BinaryReference::from_source(
        brain_path.clone(),
        ParticipantContract {
            framework: FrameworkVersion::CURRENT,
            id: brain_id.clone(),
            kind: ParticipantKind::Brain,
            requirement: None,
            config_schema: serde_json::json!({"type":"null"}),
        },
        &brain_source,
    )
    .expect("brain test binary hashes");
    let brain_participant = RuntimeParticipant::new(
        ParticipantId::new("brain").expect("brain participant id"),
        brain_id.clone(),
        None,
        None,
        ParticipantClock::Real,
    );
    binaries.insert(binary_path, drive_source);
    binaries.insert(brain_path, brain_source);
    let mut artifacts = BTreeMap::new();
    artifacts.insert(artifact_id, binary);
    artifacts.insert(brain_id, brain);
    let index = AssetIndex::from_bytes(&assets).expect("asset index");
    let runtime = Runtime::new(
        robot,
        artifacts,
        vec![participant, brain_participant],
        index,
        None,
    )
    .expect("runtime");
    let document = RuntimeDocument::new(runtime);
    (document, assets, binaries)
}

#[test]
fn runtime_rejects_more_participants_than_a_snapshot_can_publish() {
    let (RuntimeDocument::V0(mut runtime), _, _) = document();
    let template = runtime.participants[1].clone();
    runtime.participants = (0..=MAX_RUNTIME_PARTICIPANTS)
        .map(|index| {
            let mut participant = template.clone();
            participant.id = ParticipantId::new(format!("service-{index}")).unwrap();
            participant
        })
        .collect();
    assert!(matches!(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        ),
        Err(DocumentError::TooManyParticipants { count })
            if count == MAX_RUNTIME_PARTICIPANTS + 1
    ));
}

/// Compatibility is exact framework-version equality, so a bundle whose
/// artifacts were built from two trains has no valid launch - including two
/// patch releases of one pre-1.0 line.
#[test]
fn runtime_rejects_mixed_framework_artifacts_and_exposes_the_selected_train() {
    let (document, _, _) = document();
    assert_eq!(document.framework(), FrameworkVersion::CURRENT);

    let RuntimeDocument::V0(mut runtime) = document;
    let drive = ParticipantArtifactId::new("drive").expect("drive artifact id");
    let neighbour = FrameworkVersion::new(
        FrameworkVersion::CURRENT.major(),
        FrameworkVersion::CURRENT.minor(),
        FrameworkVersion::CURRENT.patch() + 1,
    );
    runtime
        .artifacts
        .get_mut(&drive)
        .expect("drive artifact")
        .contract
        .framework = neighbour;

    assert_eq!(
        neighbour.compatibility_line(),
        FrameworkVersion::CURRENT.compatibility_line(),
        "the rejected artifact must sit on the current compatibility line"
    );
    assert!(matches!(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        ),
        Err(DocumentError::MixedFramework { expected, actual, .. })
            if expected == neighbour && actual == FrameworkVersion::CURRENT
    ));
}

fn motor_robot(left: MotorCommand, right: MotorCommand) -> Robot {
    motor_robot_with_clock(Clock::Real, left, right)
}

fn simulated_motor_robot(left: MotorCommand, right: MotorCommand) -> Robot {
    motor_robot_with_clock(Clock::Simulated, left, right)
}

fn motor_robot_with_clock(clock: Clock, left: MotorCommand, right: MotorCommand) -> Robot {
    let motor_type = |command| {
        move |motor: phoxal_model::builder::ComponentTypeBuilder| {
            motor.capability(
                "spin",
                Capability::Motor(Motor {
                    target: StructuralTarget::Joint {
                        id: JointId::new("axle"),
                    },
                    command,
                    gear_ratio: 1.0,
                    max_torque_nm: None,
                    max_velocity_radps: None,
                }),
            )
        }
    };
    RobotBuilder::new("rover")
        .clock(clock)
        .component_type("left_motor", motor_type(left))
        .component_type("right_motor", motor_type(right))
        .component("left_drive", "left_motor")
        .component("right_drive", "right_motor")
        .kinematics(Kinematics::Differential {
            left_actuators: &["left_drive.spin"],
            right_actuators: &["right_drive.spin"],
            left_encoders: &[],
            right_encoders: &[],
            wheel_radius_m: 0.1,
            wheel_base_m: 0.4,
        })
        .build()
        .expect("differential test robot is valid")
}

fn topology_robot(kind: phoxal_model::robot::KinematicKind) -> Robot {
    match kind {
        phoxal_model::robot::KinematicKind::Differential => {
            motor_robot(MotorCommand::Velocity, MotorCommand::Velocity)
        }
        phoxal_model::robot::KinematicKind::Mecanum => RobotBuilder::new("rover")
            .component_type("motor", |motor| motor.motor("spin", "axle"))
            .component("front_left", "motor")
            .component("front_right", "motor")
            .component("rear_left", "motor")
            .component("rear_right", "motor")
            .kinematics(Kinematics::Mecanum {
                front_left_actuator: "front_left.spin",
                front_right_actuator: "front_right.spin",
                rear_left_actuator: "rear_left.spin",
                rear_right_actuator: "rear_right.spin",
                wheel_radius_m: 0.1,
                wheel_base_m: 0.4,
                track_m: 0.3,
            })
            .build()
            .expect("mecanum test robot is valid"),
        phoxal_model::robot::KinematicKind::Ackermann => RobotBuilder::new("rover")
            .component_type("motor", |motor| motor.motor("spin", "axle"))
            .component("drive", "motor")
            .kinematics(Kinematics::Ackermann {
                steering_actuator: "drive.spin",
                drive_actuator: "drive.spin",
                steering_encoder: None,
                drive_encoder: None,
                wheel_base_m: 0.4,
                track_m: 0.3,
                max_steering_angle_rad: 0.6,
            })
            .build()
            .expect("ackermann test robot is valid"),
        phoxal_model::robot::KinematicKind::Omnidirectional => RobotBuilder::new("rover")
            .kinematics(Kinematics::Omnidirectional {
                actuators: &[],
                encoders: &[],
            })
            .build()
            .expect("omnidirectional test robot is valid"),
    }
}

fn requirement_document(robot: Robot) -> StagedBytes {
    let (document, assets, binaries) = document();
    let RuntimeDocument::V0(mut runtime) = document;
    runtime.robot = robot;
    runtime
        .artifacts
        .values_mut()
        .next()
        .unwrap()
        .contract
        .requirement = Some(ParticipantRequirement::DifferentialDriveVelocity);
    let document = RuntimeDocument::new(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        )
        .expect("requirement runtime is valid"),
    );
    (document, assets, binaries)
}

#[test]
fn stock_drive_requirement_accepts_differential_velocity_motors() {
    let (document, _, _) =
        requirement_document(motor_robot(MotorCommand::Velocity, MotorCommand::Velocity));
    assert_eq!(
        document.robot().motion().kinematic().kind(),
        phoxal_model::robot::KinematicKind::Differential
    );
}

#[test]
fn final_runtime_rejects_a_simulator_on_a_real_robot() {
    let (document, _, _) = document();
    let RuntimeDocument::V0(mut runtime) = document;
    runtime
        .artifacts
        .get_mut(&ParticipantArtifactId::new("drive").expect("drive artifact"))
        .expect("drive artifact")
        .contract
        .kind = ParticipantKind::Simulator;

    assert!(matches!(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        ),
        Err(DocumentError::ExecutionModeMismatch {
            kind: ParticipantKind::Simulator,
            robot: Clock::Real,
            ..
        })
    ));
}

#[test]
fn final_runtime_rejects_a_driver_on_a_simulated_robot() {
    let (document, _, _) = document();
    let RuntimeDocument::V0(mut runtime) = document;
    runtime.robot = simulated_motor_robot(MotorCommand::Velocity, MotorCommand::Velocity);
    runtime
        .artifacts
        .get_mut(&ParticipantArtifactId::new("drive").expect("drive artifact"))
        .expect("drive artifact")
        .contract
        .kind = ParticipantKind::Driver;
    runtime.participants[0].component =
        Some(ComponentInstanceId::new("left_drive").expect("simulated test component instance"));
    for participant in &mut runtime.participants {
        participant.clock = ParticipantClock::Simulation;
    }

    assert!(matches!(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        ),
        Err(DocumentError::ExecutionModeMismatch {
            kind: ParticipantKind::Driver,
            robot: Clock::Simulated,
            ..
        })
    ));
}

#[test]
fn final_runtime_requires_a_simulator_to_follow_simulation_time() {
    let (document, _, _) = document();
    let RuntimeDocument::V0(mut runtime) = document;
    runtime.robot = simulated_motor_robot(MotorCommand::Velocity, MotorCommand::Velocity);
    runtime
        .artifacts
        .get_mut(&ParticipantArtifactId::new("drive").expect("drive artifact"))
        .expect("drive artifact")
        .contract
        .kind = ParticipantKind::Simulator;
    runtime.participants[0].component = None;
    runtime.participants[0].clock = ParticipantClock::Clockless;
    runtime.participants[1].clock = ParticipantClock::Simulation;

    assert!(matches!(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        ),
        Err(DocumentError::ExecutionModeMismatch {
            kind: ParticipantKind::Simulator,
            robot: Clock::Simulated,
            participant_clock: ParticipantClock::Clockless,
            ..
        })
    ));
}

#[test]
fn simulated_runtime_requires_exactly_one_simulator_authority() {
    let (document, _, _) = document();
    let RuntimeDocument::V0(mut runtime) = document;
    runtime.robot = simulated_motor_robot(MotorCommand::Velocity, MotorCommand::Velocity);
    for participant in &mut runtime.participants {
        participant.clock = ParticipantClock::Simulation;
    }

    assert!(matches!(
        Runtime::new(
            runtime.robot.clone(),
            runtime.artifacts.clone(),
            runtime.participants.clone(),
            runtime.assets.clone(),
            runtime.router.clone(),
        ),
        Err(DocumentError::MissingSimulator)
    ));

    let simulator_artifact = ParticipantArtifactId::new("drive").expect("simulator artifact");
    runtime
        .artifacts
        .get_mut(&simulator_artifact)
        .expect("simulator artifact")
        .contract
        .kind = ParticipantKind::Simulator;
    runtime.participants.push(RuntimeParticipant::new(
        ParticipantId::new("second-simulator").expect("second simulator id"),
        simulator_artifact,
        None,
        None,
        ParticipantClock::Simulation,
    ));

    assert!(matches!(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        ),
        Err(DocumentError::DuplicateSimulator)
    ));
}

#[test]
fn stock_drive_requirement_rejects_non_differential_topologies() {
    for kind in [
        phoxal_model::robot::KinematicKind::Mecanum,
        phoxal_model::robot::KinematicKind::Omnidirectional,
        phoxal_model::robot::KinematicKind::Ackermann,
    ] {
        let runtime = {
            let (base, assets, binaries) = document();
            let RuntimeDocument::V0(mut runtime) = base;
            runtime.robot = topology_robot(kind);
            runtime
                .artifacts
                .values_mut()
                .next()
                .unwrap()
                .contract
                .requirement = Some(ParticipantRequirement::DifferentialDriveVelocity);
            let _ = (assets, binaries);
            runtime
        };
        assert!(matches!(
            Runtime::new(
                runtime.robot,
                runtime.artifacts,
                runtime.participants,
                runtime.assets,
                runtime.router,
            ),
            Err(DocumentError::RequirementKinematicsMismatch { .. })
        ));
    }
}

#[test]
fn stock_drive_requirement_rejects_each_nonvelocity_drive_side() {
    for (left, right, actuator) in [
        (
            MotorCommand::Position,
            MotorCommand::Velocity,
            "left_drive.spin",
        ),
        (
            MotorCommand::Velocity,
            MotorCommand::Torque,
            "right_drive.spin",
        ),
    ] {
        let (base, _, _) = document();
        let RuntimeDocument::V0(mut runtime) = base;
        runtime.robot = motor_robot(left, right);
        runtime
            .artifacts
            .values_mut()
            .next()
            .unwrap()
            .contract
            .requirement = Some(ParticipantRequirement::DifferentialDriveVelocity);
        assert!(matches!(
            Runtime::new(
                runtime.robot,
                runtime.artifacts,
                runtime.participants,
                runtime.assets,
                runtime.router,
            ),
            Err(DocumentError::RequirementMotorModeMismatch { actuator: ref found, .. })
                if found.to_string() == actuator
        ));
    }
}

#[test]
fn paths_and_digests_are_strict() {
    for (value, valid) in [
        ("assets/mesh.obj", true),
        ("bin/brain", true),
        ("", false),
        ("/etc/passwd", false),
        ("assets/../bin/brain", false),
        ("assets//mesh.obj", false),
        ("assets\\mesh.obj", false),
    ] {
        assert_eq!(BundlePath::new(value).is_ok(), valid, "{value}");
    }
    let digest = Sha256Digest::of(b"hello");
    assert_eq!(
        Sha256Digest::from_reader(std::io::Cursor::new(b"hello")).expect("reader hashes"),
        digest
    );
    assert_eq!(Sha256Digest::parse(&digest.as_hex()), Ok(digest));
    assert!(Sha256Digest::parse(&digest.as_hex().to_uppercase()).is_err());
}

#[test]
fn runtime_json_is_tagged_strict_and_robot_id_is_persisted_once() {
    let (document, _, _) = document();
    let value = serde_json::to_value(&document).expect("document serializes");
    assert_eq!(value["schema"], RUNTIME_SCHEMA);
    assert_eq!(
        value["artifacts"]["drive"]["contract"]["requirement"],
        serde_json::Value::Null,
        "runtime artifacts persist an explicit requirement value"
    );
    assert!(value.get("robot_id").is_none());
    let text = serde_json::to_string(&document).expect("document serializes");
    assert_eq!(text.matches("\"id\":\"rover\"").count(), 1);
    assert!(
        serde_json::from_str::<RuntimeDocument>(
            &text.replace("phoxal/runtime-bundle/v0", "phoxal/runtime-bundle/v1")
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<RuntimeDocument>(
            &text.replace("\"router\":null", "\"router\":null,\"source\":true")
        )
        .is_err()
    );

    let mut duplicate = serde_json::to_value(&document).expect("document serializes");
    let participants = duplicate["participants"]
        .as_array_mut()
        .expect("participants are an array");
    participants.push(participants[0].clone());
    assert!(
        serde_json::from_value::<RuntimeDocument>(duplicate).is_err(),
        "direct runtime.json decoding must validate the participant graph"
    );
}

#[test]
fn runtime_graph_requires_exactly_one_fixed_brain_instance() {
    let (base, _, _) = document();
    let RuntimeDocument::V0(mut runtime) = base.clone();
    runtime
        .participants
        .retain(|participant| participant.id.as_str() != "brain");
    assert!(matches!(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        ),
        Err(DocumentError::MissingBrain)
    ));

    let RuntimeDocument::V0(mut runtime) = base;
    let brain = runtime
        .participants
        .iter_mut()
        .find(|participant| participant.id.as_str() == "brain")
        .expect("brain participant");
    brain.id = ParticipantId::new("other-brain").expect("participant id");
    assert!(matches!(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        ),
        Err(DocumentError::BrainIdMismatch { .. })
    ));

    let (artifact_base, _, _) = document();
    let RuntimeDocument::V0(mut runtime) = artifact_base;
    let brain_id = ParticipantArtifactId::new("brain").expect("brain artifact id");
    let mut brain = runtime.artifacts.remove(&brain_id).expect("brain artifact");
    let wrong_id = ParticipantArtifactId::new("other-brain").expect("artifact id");
    brain.contract.id = wrong_id.clone();
    brain.path = BundlePath::new("bin/other-brain").expect("binary path");
    runtime.artifacts.insert(wrong_id.clone(), brain);
    runtime
        .participants
        .iter_mut()
        .find(|participant| participant.id.as_str() == "brain")
        .expect("brain participant")
        .artifact = wrong_id;
    assert!(matches!(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        ),
        Err(DocumentError::BrainArtifactId { .. })
    ));

    let (path_base, _, _) = document();
    let RuntimeDocument::V0(mut runtime) = path_base;
    runtime
        .artifacts
        .get_mut(&ParticipantArtifactId::new("brain").expect("brain artifact id"))
        .expect("brain artifact")
        .path = BundlePath::new("bin/not-brain").expect("binary path");
    assert!(matches!(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        ),
        Err(DocumentError::BrainArtifactPath { .. })
    ));
}

#[test]
fn multiple_participant_instances_may_share_one_artifact() {
    let (document, _, _) = document();
    let RuntimeDocument::V0(mut runtime) = document;
    let artifact = runtime.participants[0].artifact.clone();
    runtime.participants.push(RuntimeParticipant::new(
        ParticipantId::new("drive-rear").expect("participant id"),
        artifact.clone(),
        None,
        None,
        ParticipantClock::Real,
    ));

    let document = RuntimeDocument::new(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        )
        .expect("shared artifact is valid"),
    );
    assert_eq!(document.artifacts().len(), 2);
    assert_eq!(document.participants().len(), 3);
    assert_eq!(
        document
            .participants()
            .iter()
            .filter(|participant| participant.artifact == artifact)
            .count(),
        2
    );
}

#[test]
fn every_artifact_must_be_selected_by_a_runtime_participant() {
    let (document, _, _) = document();
    let RuntimeDocument::V0(mut runtime) = document;
    let mut unused = runtime
        .artifacts
        .get(&ParticipantArtifactId::new("drive").expect("drive artifact id"))
        .expect("fixture artifact")
        .clone();
    let unused_id = ParticipantArtifactId::new("unused").expect("artifact id");
    unused.path = BundlePath::new("bin/unused").expect("binary path");
    unused.contract.id = unused_id.clone();
    runtime.artifacts.insert(unused_id.clone(), unused);

    assert!(matches!(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        ),
        Err(DocumentError::UnusedArtifact { artifact }) if artifact == unused_id
    ));
}

#[test]
fn artifact_config_schema_is_validated_once_even_before_config_values() {
    let (document, _, _) = document();
    let RuntimeDocument::V0(mut runtime) = document;
    let artifact = runtime
        .artifacts
        .values_mut()
        .next()
        .expect("fixture artifact");
    artifact.contract.config_schema = serde_json::json!({"type": "not-a-json-schema-type"});

    assert!(matches!(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        ),
        Err(DocumentError::InvalidConfigSchema { .. })
    ));
}

#[test]
fn driver_artifacts_require_an_explicit_component_instance_binding() {
    let (document, _, _) = document();
    let RuntimeDocument::V0(mut runtime) = document;
    runtime
        .artifacts
        .values_mut()
        .next()
        .expect("fixture artifact")
        .contract
        .kind = ParticipantKind::Driver;

    assert!(matches!(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        ),
        Err(DocumentError::MissingDriverComponent { .. })
    ));
}

#[test]
fn participant_config_is_validated_against_embedded_binary_schema() {
    let (document, _, _) = document();
    let RuntimeDocument::V0(mut runtime) = document;
    runtime.participants[0].config = Some(serde_json::json!({"unexpected": true}));
    assert!(matches!(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        ),
        Err(DocumentError::InvalidConfig { .. })
    ));
}

#[test]
fn writer_and_reader_use_only_runtime_json_and_indexed_files() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let root = parent.path().join("bundle");
    let (document, assets, binaries) = document();
    let loaded = BundleWriter::write(&root, &document, &assets, &binaries)
        .expect("bundle writes and reopens");
    assert_eq!(
        loaded.root(),
        root.parent()
            .expect("bundle parent")
            .canonicalize()
            .expect("canonical parent")
            .join(root.file_name().expect("bundle name"))
    );
    assert_eq!(loaded.robot_id().as_str(), "rover");
    assert_eq!(loaded.participants().len(), 2);
    let id = AssetId::new("robot/structure.json").expect("asset id");
    assert_eq!(
        loaded.assets().read(&id).expect("asset reads"),
        b"compiled structure"
    );
    assert!(!root.join("robot.yaml").exists());
}

#[cfg(unix)]
#[test]
fn writer_stages_a_real_executable_with_canonical_mode() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let parent = tempfile::tempdir().expect("bundle parent");
    let root = parent.path().join("bundle");
    let source = parent.path().join("probe-source");
    write_executable(&source, b"#!/bin/sh\nprintf staged\n");

    let (document, assets, mut binaries) = document();
    let RuntimeDocument::V0(mut runtime) = document;
    let artifact_id = ParticipantArtifactId::new("drive").expect("artifact id");
    let existing = runtime.artifacts.get(&artifact_id).expect("drive artifact");
    let source = BinarySource::open(&source).expect("probe source opens");
    let reference =
        BinaryReference::from_source(existing.path.clone(), existing.contract.clone(), &source)
            .expect("probe reference");
    runtime.artifacts.insert(artifact_id, reference);
    let document = RuntimeDocument::new(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        )
        .expect("runtime document"),
    );
    binaries.insert(BundlePath::new("bin/drive").expect("binary path"), source);

    BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
    let staged = root.join("bin/drive");
    assert_eq!(
        std::fs::metadata(&staged)
            .expect("staged metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    let output = Command::new(&staged)
        .output()
        .expect("staged executable runs");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"staged");
    for directory in [&root, &root.join(ASSETS_DIR), &root.join(BIN_DIR)] {
        assert_eq!(
            std::fs::metadata(directory)
                .expect("bundle directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
    for file in [
        root.join(RUNTIME_FILE),
        root.join("assets/robot/structure.json"),
    ] {
        assert_eq!(
            std::fs::metadata(file)
                .expect("bundle data metadata")
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }
}

#[test]
fn verified_and_selected_open_require_both_layout_directories() {
    for directory in [ASSETS_DIR, BIN_DIR] {
        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let (document, assets, binaries) = document();
        BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
        std::fs::remove_dir_all(root.join(directory)).expect("remove required layout directory");

        assert!(matches!(
            RuntimeBundle::open_verified(&root),
            Err(BundleError::MissingFile { .. })
        ));
        assert!(matches!(
            ParticipantBundle::open(&root, &ParticipantId::new("drive").expect("participant id")),
            Err(BundleError::MissingFile { .. })
        ));
    }
}

#[test]
fn selection_is_exact_and_happens_before_any_runtime_side_effect() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let root = parent.path().join("bundle");
    let (document, assets, binaries) = document();
    let loaded = BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
    let unknown = ParticipantId::new("missing").expect("valid unknown id");
    assert!(matches!(
        loaded.participant(&unknown),
        Err(SelectionError::Unknown { .. })
    ));
}

#[test]
fn participant_open_skips_unrelated_artifact_hashes_but_full_open_does_not() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let root = parent.path().join("bundle");
    let (document, assets, mut binaries) = document();
    let RuntimeDocument::V0(mut runtime) = document;
    let source_path = std::env::current_exe().expect("test binary path");
    let source = BinarySource::open(&source_path).expect("other source opens");
    let other_artifact = ParticipantArtifactId::new("other").expect("artifact id");
    let original = runtime
        .artifacts
        .get(&ParticipantArtifactId::new("drive").expect("artifact id"))
        .expect("drive artifact");
    let mut contract = original.contract.clone();
    contract.id = other_artifact.clone();
    let other_path = BundlePath::new("bin/other").expect("binary path");
    runtime.artifacts.insert(
        other_artifact.clone(),
        BinaryReference::from_source(other_path.clone(), contract, &source)
            .expect("other artifact"),
    );
    runtime.participants.push(RuntimeParticipant::new(
        ParticipantId::new("other").expect("participant id"),
        other_artifact,
        None,
        None,
        ParticipantClock::Real,
    ));
    binaries.insert(other_path, source);
    let document = RuntimeDocument::new(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        )
        .expect("runtime document"),
    );
    BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
    std::fs::write(root.join("bin/other"), b"changed").expect("change other artifact");

    ParticipantBundle::open(&root, &ParticipantId::new("drive").expect("participant id"))
        .expect("selected participant does not hash unrelated artifact");
    assert!(matches!(
        RuntimeBundle::open_verified(&root),
        Err(BundleError::Integrity { .. } | BundleError::Size { .. })
    ));
}

#[test]
fn participant_open_leaves_selected_image_verification_to_the_runner() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let root = parent.path().join("bundle");
    let (document, assets, binaries) = document();
    BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
    std::fs::remove_file(root.join("bin/drive")).expect("remove selected staged binary");

    ParticipantBundle::open(&root, &ParticipantId::new("drive").expect("participant id"))
        .expect("participant input loading does not consume the staged image");
    assert!(matches!(
        RuntimeBundle::open_verified(&root),
        Err(BundleError::MissingFile { .. })
    ));
}

#[test]
fn a_mutated_indexed_asset_is_rejected_on_open_and_read() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let root = parent.path().join("bundle");
    let (document, assets, binaries) = document();
    let loaded = BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
    let id = AssetId::new("robot/structure.json").expect("asset id");
    std::fs::write(root.join("assets/robot/structure.json"), b"changed").expect("change asset");
    assert!(matches!(
        loaded.assets().read(&id),
        Err(BundleError::Integrity { .. } | BundleError::Size { .. })
    ));
    assert!(matches!(
        RuntimeBundle::open_verified(&root),
        Err(BundleError::Integrity { .. } | BundleError::Size { .. })
    ));
}

#[test]
fn a_mutated_indexed_binary_is_rejected_on_open() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let root = parent.path().join("bundle");
    let (document, assets, binaries) = document();
    BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
    std::fs::write(root.join("bin/drive"), b"changed").expect("change binary");
    assert!(matches!(
        RuntimeBundle::open_verified(&root),
        Err(BundleError::Integrity { .. } | BundleError::Size { .. })
    ));
}

#[test]
fn unindexed_empty_directories_are_rejected() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let root = parent.path().join("bundle");
    let (document, assets, binaries) = document();
    BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
    std::fs::create_dir(root.join(ASSETS_DIR).join("unused")).expect("empty directory");
    assert!(matches!(
        RuntimeBundle::open_verified(&root),
        Err(BundleError::UnindexedDirectory { .. })
    ));
}

#[test]
fn a_bundle_is_published_onto_a_free_name_only() {
    for existing in ["directory", "file"] {
        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        match existing {
            "directory" => {
                std::fs::create_dir(&root).expect("existing directory");
                std::fs::write(root.join("sentinel"), b"existing").expect("sentinel");
            }
            "file" => std::fs::write(&root, b"existing").expect("existing file"),
            _ => unreachable!(),
        }

        let (document, assets, binaries) = document();
        assert!(matches!(
            BundleWriter::write(&root, &document, &assets, &binaries),
            Err(BundleError::TargetExists(_))
        ));
        assert!(
            !root.join(RUNTIME_FILE).exists(),
            "the existing target keeps its own content"
        );
    }
}

/// A source whose bytes change between indexing and staging fails the digest
/// the document recorded, and the abandoned staging directory is removed.
#[test]
fn a_source_that_changes_after_indexing_fails_the_write_and_clears_staging() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let sources = tempfile::tempdir().expect("source parent");
    let root = parent.path().join("bundle");
    let source_path = sources.path().join("drive");
    write_executable(&source_path, b"#!/bin/sh\nprintf indexed\n");

    let (document, assets, mut binaries) = document();
    let RuntimeDocument::V0(mut runtime) = document;
    let artifact_id = ParticipantArtifactId::new("drive").expect("artifact id");
    let existing = runtime.artifacts.get(&artifact_id).expect("drive artifact");
    let source = BinarySource::open(&source_path).expect("drive source opens");
    let reference =
        BinaryReference::from_source(existing.path.clone(), existing.contract.clone(), &source)
            .expect("drive reference");
    runtime.artifacts.insert(artifact_id, reference);
    let document = RuntimeDocument::new(
        Runtime::new(
            runtime.robot,
            runtime.artifacts,
            runtime.participants,
            runtime.assets,
            runtime.router,
        )
        .expect("runtime document"),
    );
    binaries.insert(BundlePath::new("bin/drive").expect("binary path"), source);
    write_executable(&source_path, b"#!/bin/sh\nprintf changed\n");

    assert!(matches!(
        BundleWriter::write(&root, &document, &assets, &binaries),
        Err(BundleError::Integrity { .. } | BundleError::Size { .. })
    ));
    assert!(!root.exists(), "no bundle was published");
    assert_eq!(
        std::fs::read_dir(parent.path())
            .expect("bundle parent reads")
            .count(),
        0,
        "the abandoned staging directory was removed"
    );
}

#[test]
fn old_source_documents_are_rejected_as_extra_bundle_truth() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let root = parent.path().join("bundle");
    let (document, assets, binaries) = document();
    BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
    std::fs::write(root.join("robot.yaml"), b"source truth").expect("old source");
    assert!(matches!(
        RuntimeBundle::open_verified(&root),
        Err(BundleError::UnexpectedFile { .. })
    ));
}
