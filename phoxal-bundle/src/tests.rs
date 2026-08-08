//! Bundle boundary regression tests.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

use crate::*;
use phoxal_model::{AssetId, Robot};
use phoxal_runtime_contract::identity::ParticipantId;
use phoxal_runtime_contract::metadata::ParticipantRequirement;

use phoxal_model::RobotBuilder;
use phoxal_model::builder::Kinematics;
use phoxal_model::component::capability::{Capability, Motor, MotorCommand, StructuralTarget};
use phoxal_model::identity::JointId;
use phoxal_runtime_contract::metadata::{ParticipantContract, ParticipantKind, ParticipantSchemas};
use phoxal_runtime_contract::version::{BusAbi, LaunchAbi, RobotApi, RuntimeSchema};

type StagedBytes = (
    RuntimeDocument,
    BTreeMap<AssetId, Vec<u8>>,
    BTreeMap<BundlePath, BinarySource>,
);

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
            id: artifact_id.clone(),
            kind: ParticipantKind::Service,
            api: RobotApi::V0_2,
            schemas: ParticipantSchemas {
                bus: BusAbi::V0,
                launch: LaunchAbi::V0,
                runtime: RuntimeSchema::V0,
            },
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
            id: brain_id.clone(),
            kind: ParticipantKind::Brain,
            api: RobotApi::V0_2,
            schemas: ParticipantSchemas {
                bus: BusAbi::V0,
                launch: LaunchAbi::V0,
                runtime: RuntimeSchema::V0,
            },
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

fn motor_robot(left: MotorCommand, right: MotorCommand) -> Robot {
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
    std::fs::write(&source, b"#!/bin/sh\nprintf staged\n").expect("probe source");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700))
        .expect("probe source mode");

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

#[cfg(unix)]
#[test]
fn opened_binary_source_is_pinned_across_path_substitution() {
    use std::os::unix::fs::PermissionsExt;

    let parent = tempfile::tempdir().expect("source parent");
    let source_path = parent.path().join("source");
    std::fs::write(&source_path, b"#!/bin/sh\nprintf original\n").expect("source bytes");
    std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o700))
        .expect("source mode");
    let source = BinarySource::open(&source_path).expect("source descriptor");
    let moved = parent.path().join("moved");
    std::fs::rename(&source_path, &moved).expect("move opened inode");
    let replacement = parent.path().join("replacement");
    std::fs::write(&replacement, b"#!/bin/sh\nprintf replacement\n").expect("replacement");
    std::os::unix::fs::symlink(&replacement, &source_path).expect("replacement symlink");
    assert!(matches!(
        BinarySource::open(&source_path),
        Err(BundleError::ForbiddenSymlink { .. })
    ));

    let (document, assets, mut binaries) = document();
    let RuntimeDocument::V0(mut runtime) = document;
    let artifact_id = ParticipantArtifactId::new("drive").expect("artifact id");
    let existing = runtime.artifacts.get(&artifact_id).expect("drive artifact");
    let reference =
        BinaryReference::from_source(existing.path.clone(), existing.contract.clone(), &source)
            .expect("reference hashes the opened inode");
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
    let root = parent.path().join("bundle");
    BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
    assert!(
        std::fs::read(root.join("bin/drive"))
            .expect("staged binary")
            .windows(b"original".len())
            .any(|window| window == b"original")
    );
}

#[cfg(unix)]
#[test]
fn verified_open_requires_the_canonical_executable_mode() {
    use std::os::unix::fs::PermissionsExt;

    let parent = tempfile::tempdir().expect("bundle parent");
    let root = parent.path().join("bundle");
    let (document, assets, binaries) = document();
    BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
    let binary = root.join("bin/drive");
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))
        .expect("change the canonical mode");
    assert!(matches!(
        RuntimeBundle::open_verified(&root),
        Err(BundleError::ExecutableMode {
            expected: 0o755,
            actual: 0o700,
            ..
        })
    ));
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
    std::fs::write(root.join("bin/other"), b"tampered").expect("tamper other artifact");

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
    std::fs::write(root.join("assets/robot/structure.json"), b"tampered").expect("tamper asset");
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
    std::fs::write(root.join("bin/drive"), b"tampered").expect("tamper binary");
    assert!(matches!(
        RuntimeBundle::open_verified(&root),
        Err(BundleError::Integrity { .. } | BundleError::Size { .. })
    ));
}

#[cfg(unix)]
#[test]
fn symlinked_bundle_files_are_rejected_before_runtime_use() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let root = parent.path().join("bundle");
    let outside = tempfile::tempdir().expect("outside root");
    let (document, assets, binaries) = document();
    let loaded = BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
    let asset = root.join("assets/robot/structure.json");
    std::fs::remove_file(&asset).expect("remove indexed asset");
    std::fs::write(outside.path().join("structure.json"), b"outside").expect("outside asset");
    std::os::unix::fs::symlink(outside.path().join("structure.json"), &asset)
        .expect("symlink asset");
    assert!(matches!(
        RuntimeBundle::open_verified(&root),
        Err(BundleError::ForbiddenSymlink { .. })
    ));
    assert!(matches!(
        loaded
            .assets()
            .read(&AssetId::new("robot/structure.json").expect("asset id")),
        Err(BundleError::ForbiddenSymlink { .. })
    ));
}

#[cfg(unix)]
#[test]
fn substituted_assets_directory_is_not_followed_by_asset_access() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let root = parent.path().join("bundle");
    let outside = tempfile::tempdir().expect("outside root");
    let (document, assets, binaries) = document();
    let loaded = BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
    let assets_dir = root.join(ASSETS_DIR);
    let moved_assets = outside.path().join(ASSETS_DIR);
    std::fs::rename(&assets_dir, &moved_assets).expect("move indexed assets");
    std::os::unix::fs::symlink(&moved_assets, &assets_dir).expect("symlink assets directory");
    assert!(matches!(
        loaded
            .assets()
            .read(&AssetId::new("robot/structure.json").expect("asset id")),
        Err(BundleError::ForbiddenSymlink { .. })
    ));
    assert!(matches!(
        RuntimeBundle::open_verified(&root),
        Err(BundleError::ForbiddenSymlink { .. })
    ));
}

#[cfg(unix)]
#[test]
fn pinned_root_cannot_be_redirected_by_root_symlink_substitution() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let root = parent.path().join("bundle");
    let moved = parent.path().join("bundle-original");
    let outside = parent.path().join("outside");
    let bundle_path = BundlePath::new("assets/asset").expect("bundle path");
    std::fs::create_dir_all(root.join(ASSETS_DIR)).expect("bundle assets");
    std::fs::create_dir_all(outside.join(ASSETS_DIR)).expect("outside assets");
    std::fs::write(root.join("assets/asset"), b"pinned").expect("pinned asset");
    std::fs::write(outside.join("assets/asset"), b"redirected").expect("outside asset");

    let pinned = BundleRoot::open(&root).expect("pin bundle root");
    std::fs::rename(&root, &moved).expect("move original bundle");
    std::os::unix::fs::symlink(&outside, &root).expect("substitute root symlink");

    let mut file = open_bundle_file(&pinned, &bundle_path).expect("open pinned asset");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).expect("read pinned asset");
    assert_eq!(bytes, b"pinned");
}

#[cfg(unix)]
#[test]
fn layout_validation_stays_on_pinned_tree_after_root_substitution() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let root = parent.path().join("bundle");
    let moved = parent.path().join("bundle-original");
    let outside = parent.path().join("outside");
    let (document, assets, binaries) = document();
    let loaded = BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
    drop(loaded);
    std::fs::write(root.join(ASSETS_DIR).join("unexpected"), b"extra")
        .expect("extra original file");

    // Build a pathname replacement containing the expected files but not
    // the extra file. A pathname-based validator would accept this tree;
    // the pinned descriptor must continue to observe the original.
    let RuntimeDocument::V0(runtime) = &document;
    std::fs::create_dir_all(outside.join(ASSETS_DIR).join("robot")).expect("outside assets");
    std::fs::create_dir_all(outside.join(BIN_DIR)).expect("outside binaries");
    std::fs::write(
        outside.join(RUNTIME_FILE),
        serde_json::to_vec_pretty(&document).expect("runtime json"),
    )
    .expect("outside runtime");
    for (id, bytes) in &assets {
        std::fs::write(
            outside
                .join(ASSETS_DIR)
                .join(id.as_str().split('/').collect::<PathBuf>()),
            bytes,
        )
        .expect("outside asset");
    }
    for (path, source) in &binaries {
        std::fs::copy(source.path(), path.filesystem_path(&outside)).expect("outside binary");
    }

    let pinned = BundleRoot::open(&root).expect("pin original root");
    std::fs::rename(&root, &moved).expect("move original root");
    std::os::unix::fs::symlink(&outside, &root).expect("substitute root symlink");

    assert!(matches!(
        validate_layout(&pinned, runtime),
        Err(BundleError::UnexpectedFile { path }) if path.ends_with("assets/unexpected")
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn no_replace_publication_preserves_target_created_after_staging() {
    let parent = tempfile::tempdir().expect("publication parent");
    let staged = parent.path().join(".bundle.staging");
    let target = parent.path().join("bundle");
    std::fs::create_dir(&staged).expect("staging directory");
    std::fs::write(staged.join("new"), b"new bundle").expect("staged marker");

    // This target is created after staging, standing in for a concurrent
    // publisher winning the check-to-publish race.
    std::fs::create_dir(&target).expect("concurrent target");
    std::fs::write(target.join("sentinel"), b"existing bundle").expect("target sentinel");

    assert!(matches!(
        publish_staging_root(&staged, &target),
        Err(BundleError::TargetExists(path)) if path == target
    ));
    assert_eq!(
        std::fs::read(target.join("sentinel")).expect("sentinel remains"),
        b"existing bundle"
    );
    assert!(
        staged.exists(),
        "failed publication retains staging for cleanup"
    );
    assert!(!target.join("new").exists(), "target was never replaced");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn no_replace_publication_closes_the_preflight_race_for_all_target_types() {
    for target_kind in ["directory", "file", "symlink"] {
        let parent = tempfile::tempdir().expect("publication parent");
        let staged = parent.path().join(".bundle.staging");
        let target = parent.path().join("bundle");
        std::fs::create_dir(&staged).expect("staging directory");
        std::fs::write(staged.join("new"), b"new bundle").expect("staged marker");

        // Model the real writer sequence: the advisory preflight observes
        // an absent target, then a concurrent actor creates it immediately
        // before the no-replace publish syscall.
        assert!(matches!(reject_existing_target(&target), Ok(())));
        match target_kind {
            "directory" => {
                std::fs::create_dir(&target).expect("concurrent directory");
                std::fs::write(target.join("sentinel"), b"directory").expect("directory sentinel");
            }
            "file" => std::fs::write(&target, b"file").expect("concurrent file"),
            "symlink" => {
                let outside = parent.path().join("outside");
                std::fs::write(&outside, b"outside").expect("outside target");
                std::os::unix::fs::symlink(&outside, &target).expect("concurrent symlink");
            }
            _ => unreachable!(),
        }

        assert!(matches!(
            publish_staging_root(&staged, &target),
            Err(BundleError::TargetExists(path)) if path == target
        ));
        assert!(
            staged.exists(),
            "failed publication retains staging for cleanup"
        );
        assert!(!target.join("new").exists(), "target was never replaced");
        match target_kind {
            "directory" => assert_eq!(
                std::fs::read(target.join("sentinel")).expect("sentinel remains"),
                b"directory"
            ),
            "file" => assert_eq!(std::fs::read(&target).expect("file remains"), b"file"),
            "symlink" => assert!(
                std::fs::symlink_metadata(&target)
                    .expect("symlink remains")
                    .file_type()
                    .is_symlink()
            ),
            _ => unreachable!(),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn writer_tail_race_removes_staging_without_touching_new_targets() {
    for target_kind in ["directory", "file", "symlink"] {
        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let target = root
            .parent()
            .expect("bundle parent path")
            .canonicalize()
            .expect("canonical bundle parent")
            .join("bundle");
        let (document, assets, binaries) = document();
        let observed_staging = std::cell::RefCell::new(None);

        let result =
            BundleWriter::write_inner(&root, &document, &assets, &binaries, |staged, target| {
                *observed_staging.borrow_mut() = Some(staged.to_path_buf());
                match target_kind {
                    "directory" => {
                        std::fs::create_dir(target).expect("concurrent directory");
                        std::fs::write(target.join("sentinel"), b"directory")
                            .expect("directory sentinel");
                    }
                    "file" => std::fs::write(target, b"file").expect("concurrent file"),
                    "symlink" => {
                        let outside = parent.path().join("outside");
                        std::fs::write(&outside, b"outside").expect("outside target");
                        std::os::unix::fs::symlink(&outside, target).expect("concurrent symlink");
                    }
                    _ => unreachable!(),
                }
                publish_staging_root(staged, target)
            });

        assert!(matches!(
            result,
            Err(BundleError::TargetExists(path)) if path == target
        ));
        let staging = observed_staging
            .into_inner()
            .expect("writer reached publication tail");
        assert!(
            !staging.exists(),
            "writer removes failed task-owned staging"
        );
        assert!(
            !target.join("runtime.json").exists(),
            "target was never replaced"
        );
        match target_kind {
            "directory" => assert_eq!(
                std::fs::read(target.join("sentinel")).expect("sentinel remains"),
                b"directory"
            ),
            "file" => assert_eq!(std::fs::read(&target).expect("file remains"), b"file"),
            "symlink" => assert!(
                std::fs::symlink_metadata(&target)
                    .expect("symlink remains")
                    .file_type()
                    .is_symlink()
            ),
            _ => unreachable!(),
        }
    }
}

#[cfg(not(unix))]
#[test]
fn unsupported_platforms_fail_closed_for_asset_open() {
    let root = tempfile::tempdir().expect("bundle root");
    let path = root.path().join(ASSETS_DIR).join("asset");
    std::fs::create_dir_all(path.parent().expect("asset parent")).expect("asset directory");
    std::fs::write(&path, b"asset").expect("asset file");
    let bundle_path = BundlePath::new("assets/asset").expect("bundle path");
    assert!(matches!(
        BundleRoot::open(root.path()),
        Err(BundleError::UnsupportedSecureOpen { .. })
    ));
}

#[cfg(unix)]
#[test]
fn writer_rejects_existing_symlinked_target_before_writing() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let outside = tempfile::tempdir().expect("outside root");
    let root = parent.path().join("bundle");
    std::fs::create_dir(&root).expect("existing root");
    std::os::unix::fs::symlink(outside.path(), root.join(ASSETS_DIR)).expect("assets symlink");
    let (document, assets, binaries) = document();
    assert!(matches!(
        BundleWriter::write(&root, &document, &assets, &binaries),
        Err(BundleError::ForbiddenSymlink { .. })
    ));
    assert!(!outside.path().join("robot").exists());
}

#[cfg(unix)]
#[test]
fn writer_rejects_symlinked_leaf_in_existing_target() {
    let parent = tempfile::tempdir().expect("bundle parent");
    let outside = tempfile::tempdir().expect("outside root");
    let root = parent.path().join("bundle");
    std::fs::create_dir_all(root.join(ASSETS_DIR).join("robot")).expect("existing tree");
    let leaf = root.join(ASSETS_DIR).join("robot/structure.json");
    std::os::unix::fs::symlink(outside.path().join("structure.json"), &leaf).expect("leaf symlink");
    let (document, assets, binaries) = document();
    assert!(matches!(
        BundleWriter::write(&root, &document, &assets, &binaries),
        Err(BundleError::ForbiddenSymlink { .. })
    ));
    assert!(!outside.path().join("structure.json").exists());
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
