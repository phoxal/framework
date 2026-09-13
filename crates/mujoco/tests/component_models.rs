//! Native closure compilation for every maintained framework component model.

#![cfg(feature = "native")]

use std::fs;
use std::path::Path;

use phoxal_component_bno085 as bno085_contract;
use phoxal_component_ddsm115 as ddsm115_contract;
use phoxal_component_oak_d_lite as oak_contract;
use phoxal_component_vl53l1x as vl53l1x_contract;
use phoxal_component_zed_f9p as zed_contract;
use phoxal_motion as motion_contract;
use phoxal_mujoco::{Model, Scene};
use phoxal_port::PortDescriptor;

fn assert_sensor_binding<P: PortDescriptor>(model: &Model, port: P, native_sensor: &str) {
    let binding = model
        .bind_sensor(port, native_sensor)
        .unwrap_or_else(|error| panic!("{} binding must be valid: {error}", port.name()));
    let info = binding.info;
    assert_eq!(binding.port, port.signature());
    assert!(
        info.dimension > 0,
        "{} native sensor {native_sensor} must emit data",
        port.name()
    );
}

fn assert_site_binding<P: PortDescriptor>(model: &Model, port: P, native_site: &str) {
    let binding = model
        .bind_site(port, native_site)
        .unwrap_or_else(|error| panic!("{} binding must be valid: {error}", port.name()));
    assert_eq!(binding.port, port.signature());
}

fn assert_camera_binding<P: PortDescriptor>(model: &Model, port: P, native_camera: &str) {
    let binding = model
        .bind_camera(port, native_camera)
        .unwrap_or_else(|error| panic!("{} binding must be valid: {error}", port.name()));
    let info = binding.info;
    assert_eq!(binding.port, port.signature());
    assert!(
        info.resolution.iter().all(|dimension| *dimension > 0),
        "{} native camera {native_camera} must have a render resolution",
        port.name()
    );
}

fn assert_actuator_binding<P: PortDescriptor>(model: &Model, port: P, native_actuator: &str) {
    let binding = model
        .bind_actuator(port, native_actuator)
        .unwrap_or_else(|error| panic!("{} binding must be valid: {error}", port.name()));
    let info = binding.info;
    assert_eq!(binding.port, port.signature());
    assert!(
        info.control_range
            .is_some_and(|range| range[0].is_finite() && range[1].is_finite()),
        "{} native actuator {native_actuator} must have finite limits",
        port.name()
    );
}

#[test]
fn official_component_models_compile_from_their_closed_directories() {
    let components = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components");
    for component in ["bno085", "ddsm115", "oak_d_lite", "vl53l1x", "zed_f9p"] {
        let root = components.join(component);
        let model = Model::from_file(root.join("model.xml"))
            .unwrap_or_else(|error| panic!("{component}/model.xml must compile: {error}"));
        assert_eq!(model.artifact().entry(), "model.xml");
        assert!(
            model.artifact().resource("model.xml").is_some(),
            "{component}/model.xml must be part of its closed resource set"
        );
    }
}

#[test]
fn component_models_leave_the_scene_physics_quantum_to_composition() {
    let components = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components");
    let mut timesteps = Vec::new();

    for component in ["bno085", "ddsm115", "oak_d_lite", "vl53l1x", "zed_f9p"] {
        let root = components.join(component);
        let source = fs::read_to_string(root.join("model.xml"))
            .unwrap_or_else(|error| panic!("{component}/model.xml must be readable: {error}"));
        assert!(
            !source
                .lines()
                .any(|line| line.trim_start().starts_with("<option")),
            "{component}/model.xml must not own the composed scene physics quantum"
        );

        let model = Model::from_file(root.join("model.xml"))
            .unwrap_or_else(|error| panic!("{component}/model.xml must compile: {error}"));
        timesteps.push(model.timestep());
    }

    assert!(
        timesteps
            .windows(2)
            .all(|pair| (pair[0] - pair[1]).abs() < f64::EPSILON),
        "component models must compile with one simulator-owned default quantum: {timesteps:?}"
    );
}

#[test]
fn official_models_keep_capability_targets_and_native_signal_names() {
    let components = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components");

    let bno085 = Model::from_file(components.join("bno085/model.xml")).expect("BNO085 model");
    assert!(bno085.body("sensor_link").unwrap().is_some());
    assert!(bno085.site("sensor_site").unwrap().is_some());
    for sensor in ["imu_orientation", "accelerometer", "gyroscope"] {
        assert!(
            bno085.sensor(sensor).unwrap().is_some(),
            "BNO085 signal {sensor} must remain model-owned"
        );
    }
    assert_sensor_binding(&bno085, bno085_contract::ports::IMU, "imu_orientation");
    assert_sensor_binding(
        &bno085,
        bno085_contract::ports::ACCELEROMETER,
        "accelerometer",
    );
    assert_sensor_binding(&bno085, bno085_contract::ports::GYROSCOPE, "gyroscope");

    let ddsm115 = Model::from_file(components.join("ddsm115/model.xml")).expect("DDSM115 model");
    assert!(ddsm115.joint("motor_joint").unwrap().is_some());
    assert!(ddsm115.actuator("motor").unwrap().is_some());
    for sensor in ["encoder_position", "encoder_velocity"] {
        assert!(
            ddsm115.sensor(sensor).unwrap().is_some(),
            "DDSM115 signal {sensor} must remain model-owned"
        );
    }
    assert_actuator_binding(&ddsm115, motion_contract::ports::ACTUATORS, "motor");
    assert_sensor_binding(
        &ddsm115,
        ddsm115_contract::ports::ENCODER,
        "encoder_position",
    );
    assert_sensor_binding(
        &ddsm115,
        ddsm115_contract::ports::ENCODER,
        "encoder_velocity",
    );
    assert!(
        ddsm115
            .artifact()
            .resource("assets/meshes/ddsm115.obj")
            .is_some()
    );
    assert!(
        ddsm115
            .artifact()
            .resource("assets/meshes/motorized_wheel.mtl")
            .is_some()
    );

    let oak = Model::from_file(components.join("oak_d_lite/model.xml")).expect("OAK-D Lite model");
    for site in [
        "left_mono_site",
        "rgb_site",
        "right_mono_site",
        "stereo_center_site",
        "imu_site",
    ] {
        assert!(
            oak.site(site).unwrap().is_some(),
            "OAK-D Lite capability target {site} must remain model-owned"
        );
    }
    for sensor in ["imu_orientation", "accelerometer", "gyroscope"] {
        assert!(
            oak.sensor(sensor).unwrap().is_some(),
            "OAK-D Lite signal {sensor} must remain model-owned"
        );
    }
    assert_camera_binding(&oak, oak_contract::ports::LEFT_MONO, "left_mono");
    assert_camera_binding(&oak, oak_contract::ports::RGB, "rgb");
    assert_camera_binding(&oak, oak_contract::ports::RIGHT_MONO, "right_mono");
    assert_camera_binding(&oak, oak_contract::ports::DEPTH, "depth");
    assert_sensor_binding(&oak, oak_contract::ports::IMU, "imu_orientation");
    assert_sensor_binding(&oak, oak_contract::ports::ACCELEROMETER, "accelerometer");
    assert_sensor_binding(&oak, oak_contract::ports::GYROSCOPE, "gyroscope");

    let vl53l1x = Model::from_file(components.join("vl53l1x/model.xml")).expect("VL53L1X model");
    assert!(vl53l1x.body("sensor_link").unwrap().is_some());
    assert!(vl53l1x.site("sensor_site").unwrap().is_some());
    assert!(vl53l1x.sensor("range").unwrap().is_some());
    assert_sensor_binding(&vl53l1x, vl53l1x_contract::ports::RANGE, "range");

    let zed = Model::from_file(components.join("zed_f9p/model.xml")).expect("ZED-F9P model");
    assert!(zed.body("sensor_link").unwrap().is_some());
    assert!(zed.site("sensor_site").unwrap().is_some());
    assert_site_binding(&zed, zed_contract::ports::GNSS, "sensor_site");
}

#[test]
fn native_bindings_fail_closed_for_wrong_kinds_and_missing_objects() {
    let components = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components");
    let bno085 = Model::from_file(components.join("bno085/model.xml")).expect("BNO085 model");

    let wrong_kind = bno085
        .bind_sensor(motion_contract::ports::ACTUATORS, "accelerometer")
        .expect_err("a consuming setpoint cannot serve as a sensor sample");
    assert!(matches!(
        wrong_kind,
        phoxal_mujoco::ModelError::InvalidBindingKind {
            native_kind: "sensor",
            ..
        }
    ));

    let missing = bno085
        .bind_sensor(bno085_contract::ports::IMU, "not_in_the_model")
        .expect_err("a missing native source must not be fabricated");
    assert!(matches!(
        missing,
        phoxal_mujoco::ModelError::MissingBinding {
            native_kind: "sensor",
            ..
        }
    ));
}

#[test]
fn native_bindings_read_only_from_their_own_model_snapshot() {
    let components = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components");

    let bno085 = Model::from_file(components.join("bno085/model.xml")).expect("BNO085 model");
    let bno085_snapshot = Scene::new(bno085.clone())
        .expect("BNO085 scene")
        .snapshot()
        .expect("BNO085 snapshot");
    let imu = bno085
        .bind_sensor(bno085_contract::ports::IMU, "imu_orientation")
        .expect("BNO085 IMU binding");
    assert_eq!(
        imu.values(&bno085_snapshot).unwrap().len(),
        imu.info.dimension
    );

    let ddsm115 = Model::from_file(components.join("ddsm115/model.xml")).expect("DDSM115 model");
    let ddsm115_snapshot = Scene::new(ddsm115.clone())
        .expect("DDSM115 scene")
        .snapshot()
        .expect("DDSM115 snapshot");
    let actuator = ddsm115
        .bind_actuator(motion_contract::ports::ACTUATORS, "motor")
        .expect("DDSM115 actuator binding");
    assert_eq!(actuator.control(&ddsm115_snapshot).unwrap(), 0.0);
    let encoder = ddsm115
        .bind_sensor(ddsm115_contract::ports::ENCODER, "encoder_velocity")
        .expect("DDSM115 encoder binding");
    assert_eq!(encoder.values(&ddsm115_snapshot).unwrap().len(), 1);

    let zed = Model::from_file(components.join("zed_f9p/model.xml")).expect("ZED-F9P model");
    let zed_snapshot = Scene::new(zed.clone())
        .expect("ZED-F9P scene")
        .snapshot()
        .expect("ZED-F9P snapshot");
    let antenna = zed
        .bind_site(zed_contract::ports::GNSS, "sensor_site")
        .expect("ZED-F9P antenna binding");
    assert_eq!(antenna.position(&zed_snapshot).unwrap(), [0.0, 0.0, 0.01]);

    let vl53l1x = Model::from_file(components.join("vl53l1x/model.xml")).expect("VL53L1X model");
    let vl53l1x_snapshot = Scene::new(vl53l1x)
        .expect("VL53L1X scene")
        .snapshot()
        .expect("VL53L1X snapshot");
    assert!(imu.values(&vl53l1x_snapshot).is_err());
}
