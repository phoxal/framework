//! Proof that a second source generation ends at the normalizer.
//!
//! The product grammar has exactly one robot generation today, so nothing in
//! the shipped crate demonstrates what happens when a second one arrives. This
//! module is that demonstration, and it is test-only on purpose: the DTO below
//! is not registered in [`crate::authoring::source::robot::Manifest`], carries no schema
//! tag in the product grammar, and no authored document can select it.
//!
//! What it proves is structural. A document written in a deliberately different
//! source language - renamed keys, sequences where the current generation uses
//! maps, a driver block spelled its own way, its own defaults - normalizes into
//! exactly the same [`crate::authoring::normalized::Robot`] as the equivalent current
//! document, and compiles through the same compiler to the same canonical
//! output. A real future generation therefore costs one DTO and one
//! `normalize`, and no second copy of the compiler.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::model::CapabilityRole;
use crate::model::component::capability::CapabilityKind;
use crate::model::robot::KinematicConfig;

use crate::authoring::normalized;

/// A robot source language that is not the one the product ships.
///
/// Every difference here is deliberate: `identity` instead of `robot.id`,
/// `frame` instead of `robot.structure`, and a `mounts` sequence instead of a
/// `robot.components` map.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestAltRobotDto {
    identity: String,
    frame: PathBuf,
    drive: KinematicConfig,
    limits: AltLimits,
    #[serde(default)]
    mounts: Vec<AltMount>,
    #[serde(default)]
    programs: Vec<AltProgram>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AltLimits {
    linear_mps: f64,
    angular_radps: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AltMount {
    name: String,
    kind: String,
    link: String,
    #[serde(default)]
    hardware: Option<AltHardware>,
    #[serde(default)]
    purpose: Vec<AltPurpose>,
    #[serde(default)]
    tuning: Vec<AltTuning>,
}

/// A driver block that is spelled nothing like the current generation's, and
/// still resolves into the one shared driver vocabulary.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AltHardware {
    can_bus: u8,
    can_node: u8,
    /// The driver binary's own configuration, under this generation's own key.
    /// It stays opaque here exactly as it does in the current generation: no
    /// source language interprets it.
    #[serde(default)]
    settings: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AltPurpose {
    capability: String,
    #[serde(rename = "for")]
    roles: Vec<CapabilityRole>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AltTuning {
    capability: String,
    kind: AltTuningKind,
    /// This generation says which way a capability turns with a flag rather
    /// than a signed integer; the sign is what survives normalization.
    #[serde(default)]
    reversed: bool,
}

/// This generation only tunes the two kinds a direction actually means
/// something for, which is a grammar choice it is free to make: what leaves the
/// normalizer is the canonical kind either way.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AltTuningKind {
    Motor,
    Encoder,
}

impl AltTuningKind {
    const fn canonical(&self) -> CapabilityKind {
        match self {
            Self::Motor => CapabilityKind::Motor,
            Self::Encoder => CapabilityKind::Encoder,
        }
    }
}

impl TestAltRobotDto {
    /// This generation's own normalization: the only code a second source
    /// language actually costs.
    fn normalize(self) -> normalized::Robot {
        normalized::Robot {
            id: self.identity,
            structure: self.frame,
            kinematic: self.drive,
            motion_limits: crate::model::robot::MotionLimits {
                max_linear_speed_mps: self.limits.linear_mps,
                max_angular_speed_radps: self.limits.angular_radps,
            },
            instances: self
                .mounts
                .into_iter()
                .map(|mount| {
                    (
                        mount.name,
                        normalized::ComponentInstance {
                            component_type: mount.kind,
                            mount_link: mount.link,
                            driver: mount.hardware.map(|hardware| {
                                crate::authoring::source::robot::driver::DriverConfig {
                                    connection: crate::model::connection::Connection::Can(
                                        crate::model::connection::Can {
                                            bus: hardware.can_bus,
                                            node_id: hardware.can_node,
                                        },
                                    ),
                                    config: hardware.settings,
                                }
                            }),
                            roles: mount
                                .purpose
                                .into_iter()
                                .map(|purpose| {
                                    (purpose.capability, purpose.roles.into_iter().collect())
                                })
                                .collect(),
                            parameters: mount
                                .tuning
                                .into_iter()
                                .map(|tuning| {
                                    (
                                        tuning.capability,
                                        normalized::CapabilityParameters {
                                            kind: tuning.kind.canonical(),
                                            direction_sign: if tuning.reversed { -1 } else { 1 },
                                        },
                                    )
                                })
                                .collect(),
                        },
                    )
                })
                .collect(),
            services: self
                .programs
                .into_iter()
                .map(|program| (program.name, program.settings))
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AltProgram {
    name: String,
    #[serde(default)]
    settings: Option<serde_json::Value>,
}

/// The fixture robot, written in the alternative source language.
const ALT_DOCUMENT: &str = r#"
identity: rgbd-imu-diff-drive
frame: structure.urdf
limits:
  linear_mps: 0.6
  angular_radps: 2.0
drive:
  kind: differential
  left_actuators: [front_left_drive.motor, rear_left_drive.motor]
  right_actuators: [front_right_drive.motor, rear_right_drive.motor]
  left_encoders: [front_left_drive.encoder, rear_left_drive.encoder]
  right_encoders: [front_right_drive.encoder, rear_right_drive.encoder]
  wheel_radius_m: 0.1
  wheel_base_m: 0.4
programs:
  - name: localize
    settings:
      rate_hz: 10
mounts:
  - name: front_left_drive
    kind: drive_motor
    link: front_left_wheel_mount
    hardware: { can_bus: 0, can_node: 1, settings: { reduction: 20 } }
    tuning:
      - { capability: motor, kind: motor }
      - { capability: encoder, kind: encoder }
  - name: front_right_drive
    kind: drive_motor
    link: front_right_wheel_mount
    hardware: { can_bus: 0, can_node: 2 }
    tuning:
      - { capability: motor, kind: motor, reversed: true }
      - { capability: encoder, kind: encoder, reversed: true }
  - name: rear_left_drive
    kind: drive_motor
    link: rear_left_wheel_mount
    hardware: { can_bus: 0, can_node: 3 }
    tuning:
      - { capability: motor, kind: motor }
      - { capability: encoder, kind: encoder }
  - name: rear_right_drive
    kind: drive_motor
    link: rear_right_wheel_mount
    hardware: { can_bus: 0, can_node: 4 }
    tuning:
      - { capability: motor, kind: motor, reversed: true }
      - { capability: encoder, kind: encoder, reversed: true }
  - name: imu
    kind: imu
    link: imu_mount
    purpose:
      - { capability: imu, for: [odometry, localization] }
  - name: front_camera
    kind: camera_rgbd_640x480
    link: front_camera_mount
    purpose:
      - { capability: rgb, for: [localization] }
      - { capability: depth, for: [localization, mapping, traversability] }
  - name: front_center_tof
    kind: range_tof
    link: front_center_tof_mount
    purpose:
      - { capability: range, for: [mapping, traversability, safety] }
  - name: gnss
    kind: gnss
    link: gnss_mount
    purpose:
      - { capability: gnss, for: [localization] }
"#;

/// The same robot, in the source language the product actually ships.
const CURRENT_SERVICES: &str = "\nservices:\n  localize:\n    config:\n      rate_hz: 10\n";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .expect("this crate sits at phoxal/ under the workspace root")
        .to_path_buf()
}

/// A copy of the fixture project at `temp`, extended with a declared service so
/// the proof covers every source-owned fact the compiler carries.
fn staged_project(temp: &Path) {
    let fixture = workspace_root().join("fixture/robot/rgbd-imu-diff-drive");
    let mut document =
        std::fs::read_to_string(fixture.join("robot.yaml")).expect("the fixture robot document");
    document.push_str(CURRENT_SERVICES);
    std::fs::write(temp.join("robot.yaml"), document).expect("a writable staging directory");
    std::fs::copy(fixture.join("structure.urdf"), temp.join("structure.urdf"))
        .expect("a readable fixture file");
}

fn component_roots(robot: &normalized::Robot) -> BTreeMap<String, PathBuf> {
    let components = workspace_root().join("fixture/components");
    robot
        .used_component_types()
        .into_iter()
        .map(|component_type| (component_type.to_string(), components.join(component_type)))
        .collect()
}

/// Compile one normalized robot exactly the way [`crate::authoring::SourceSet::compile`]
/// does, so both generations go through the same compiler and not merely
/// through the same function names.
fn compile(project_root: &Path, robot: &normalized::Robot) -> crate::authoring::CompiledProject {
    let project_root = project_root
        .canonicalize()
        .expect("the staged project resolves");
    let resolved = crate::authoring::ResolvedSources {
        robot_manifest: project_root.join("robot.yaml"),
        robot_root: project_root.clone(),
        component_roots: component_roots(robot),
    };
    let model = resolved
        .compile_model(robot, OFFICIAL_SERVICES.map(official))
        .expect("the model compiles");
    let assets = resolved
        .compile_assets(&project_root, robot, &model)
        .expect("the assets compile");
    crate::authoring::CompiledProject {
        robot: model,
        assets,
    }
}

/// A caller-supplied official service set, so the proof covers the merge with
/// the authored `services:` map rather than only the authored half.
const OFFICIAL_SERVICES: [&str; 2] = ["drive", "localize"];

fn official(id: &str) -> crate::model::identity::ServiceId {
    crate::model::identity::ServiceId::new(id).expect("an official service id is a token")
}

/// What a compiled project is, as bytes, for comparison across generations.
fn rendered(project: &crate::authoring::CompiledProject) -> String {
    let mut rendered = serde_json::to_string_pretty(project.robot()).expect("the model serializes");
    rendered.push('\n');
    for (id, bytes) in project.assets().iter() {
        rendered.push_str(&format!("{} {}\n", id.as_str(), bytes.len()));
    }
    rendered
}

/// The two source languages meet at the normalized robot and nowhere later.
#[test]
fn a_second_source_generation_normalizes_into_the_same_robot() {
    let temp = tempfile::tempdir().expect("a staging directory");
    staged_project(temp.path());

    let current = crate::authoring::source::robot::Manifest::load(temp.path().join("robot.yaml"))
        .expect("the current-generation document parses")
        .normalize()
        .expect("the current-generation document normalizes");
    let alternative = serde_yaml::from_str::<TestAltRobotDto>(ALT_DOCUMENT)
        .expect("the alternative document parses")
        .normalize();

    assert_eq!(
        alternative, current,
        "a second source language must resolve into the same normalized robot"
    );
}

/// And the compiler cannot tell which of them it was handed.
#[test]
fn a_second_source_generation_compiles_to_the_same_canonical_output() {
    let temp = tempfile::tempdir().expect("a staging directory");
    staged_project(temp.path());

    let current = crate::authoring::source::robot::Manifest::load(temp.path().join("robot.yaml"))
        .expect("the current-generation document parses")
        .normalize()
        .expect("the current-generation document normalizes");
    let alternative = serde_yaml::from_str::<TestAltRobotDto>(ALT_DOCUMENT)
        .expect("the alternative document parses")
        .normalize();

    assert_eq!(
        rendered(&compile(temp.path(), &alternative)),
        rendered(&compile(temp.path(), &current)),
        "both source languages must compile to identical canonical output"
    );
}
