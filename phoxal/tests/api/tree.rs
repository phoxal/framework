//! The dynamic endpoint tree: what a walk of it renders, on both sides.
//!
//! Every assertion here is a concrete key. The templates the same declarations
//! emit are checked in [`crate::templates`], and the two cannot drift because
//! the same `nodes!`/`endpoints!` structure renders both.

use phoxal::identity::ComponentInstanceId;
use phoxal::model::identity::{CapabilityId, JointId};

use crate::{robot as api, runtime, simulation, supervisor};

fn component(value: &str) -> ComponentInstanceId {
    ComponentInstanceId::new(value).expect("a canonical component instance")
}

fn capability(value: &str) -> CapabilityId {
    CapabilityId::new(value).expect("a canonical capability")
}

fn joint(value: &str) -> JointId {
    JointId::new(value)
}

/// A fully static path: every segment is a module identifier, and the leading
/// one is the family's own id.
#[test]
fn a_static_path_renders_its_module_identifiers() {
    assert_eq!(api::topics().drive().state().key(), "robot/drive/state");
    assert_eq!(api::topics().drive().target().key(), "robot/drive/target");
    assert_eq!(api::topics().motion().manual().key(), "robot/motion/manual");
    assert_eq!(api::topics().motion().state().key(), "robot/motion/state");
    assert_eq!(
        api::topics().navigation().state().key(),
        "robot/navigation/state"
    );
    assert_eq!(
        api::topics().navigation().result().key(),
        "robot/navigation/result"
    );
    assert_eq!(
        api::topics().navigation().next_frontier().key(),
        "robot/navigation/next_frontier"
    );
    assert_eq!(
        api::topics().safety().constraints().key(),
        "robot/safety/constraints"
    );
    assert_eq!(api::topics().safety().state().key(), "robot/safety/state");
    assert_eq!(api::topics().frame().tree().key(), "robot/frame/tree");
    assert_eq!(
        api::topics().frame().static_transforms().key(),
        "robot/frame/static_transforms"
    );
    assert_eq!(api::topics().frame().lookup().key(), "robot/frame/lookup");
    assert_eq!(
        api::topics().perception().detections().key(),
        "robot/perception/detections"
    );
    assert_eq!(
        api::topics().perception().state().key(),
        "robot/perception/state"
    );
    assert_eq!(api::topics().video().open().key(), "robot/video/open");
    assert_eq!(api::topics().map().revision().key(), "robot/map/revision");
    assert_eq!(api::topics().map().submap().key(), "robot/map/submap");
    assert_eq!(
        api::topics().odometry().state().key(),
        "robot/odometry/state"
    );
    assert_eq!(
        api::topics().localize().state().key(),
        "robot/localize/state"
    );
}

/// One dynamic segment, bound from the model identity that names it.
#[test]
fn a_one_variable_path_binds_its_segment_in_place() {
    let elbow = joint("elbow");
    assert_eq!(
        api::topics()
            .joint(&elbow)
            .expect("a concrete joint segment")
            .state()
            .key(),
        "robot/joint/elbow/state"
    );
}

/// Two dynamic segments, at two different depths, each bound where it sits.
#[test]
fn a_nested_two_variable_path_binds_both_segments() {
    assert_eq!(
        api::topics()
            .component(&component("head"))
            .expect("a concrete component segment")
            .camera(&capability("front"))
            .expect("a concrete capability segment")
            .frame()
            .key(),
        "robot/component/head/camera/front/frame"
    );
}

/// Every component capability, so a capability added to the branch without a
/// key is visible here rather than only in the compatibility templates.
#[test]
fn every_component_capability_renders_its_own_key() {
    let base = component("base");
    let cap = capability("c0");
    let api = api::topics();
    let node = || api.component(&base).expect("a concrete component segment");
    for (rendered, expected) in [
        (
            node()
                .accelerometer(&cap)
                .expect("accel")
                .sample()
                .key()
                .to_owned(),
            "robot/component/base/accelerometer/c0/sample",
        ),
        (
            node()
                .battery(&cap)
                .expect("battery")
                .state()
                .key()
                .to_owned(),
            "robot/component/base/battery/c0/state",
        ),
        (
            node()
                .camera(&cap)
                .expect("camera")
                .frame()
                .key()
                .to_owned(),
            "robot/component/base/camera/c0/frame",
        ),
        (
            node().depth(&cap).expect("depth").frame().key().to_owned(),
            "robot/component/base/depth/c0/frame",
        ),
        (
            node()
                .emergency_stop(&cap)
                .expect("estop")
                .state()
                .key()
                .to_owned(),
            "robot/component/base/emergency_stop/c0/state",
        ),
        (
            node()
                .encoder(&cap)
                .expect("encoder")
                .sample()
                .key()
                .to_owned(),
            "robot/component/base/encoder/c0/sample",
        ),
        (
            node().gnss(&cap).expect("gnss").sample().key().to_owned(),
            "robot/component/base/gnss/c0/sample",
        ),
        (
            node()
                .gyroscope(&cap)
                .expect("gyro")
                .sample()
                .key()
                .to_owned(),
            "robot/component/base/gyroscope/c0/sample",
        ),
        (
            node().imu(&cap).expect("imu").sample().key().to_owned(),
            "robot/component/base/imu/c0/sample",
        ),
        (
            node().led(&cap).expect("led").command().key().to_owned(),
            "robot/component/base/led/c0/command",
        ),
        (
            node().lidar(&cap).expect("lidar").scan().key().to_owned(),
            "robot/component/base/lidar/c0/scan",
        ),
        (
            node()
                .magnetometer(&cap)
                .expect("mag")
                .sample()
                .key()
                .to_owned(),
            "robot/component/base/magnetometer/c0/sample",
        ),
        (
            node()
                .microphone(&cap)
                .expect("mic")
                .frame()
                .key()
                .to_owned(),
            "robot/component/base/microphone/c0/frame",
        ),
        (
            node().mmwave(&cap).expect("mmwave").scan().key().to_owned(),
            "robot/component/base/mmwave/c0/scan",
        ),
        (
            node()
                .motor(&cap)
                .expect("motor")
                .command()
                .key()
                .to_owned(),
            "robot/component/base/motor/c0/command",
        ),
        (
            node().range(&cap).expect("range").sample().key().to_owned(),
            "robot/component/base/range/c0/sample",
        ),
        (
            node()
                .speaker(&cap)
                .expect("speaker")
                .stream()
                .key()
                .to_owned(),
            "robot/component/base/speaker/c0/stream",
        ),
    ] {
        assert_eq!(rendered, expected);
    }
}

/// A `self` leaf binds the node path itself, so the key carries no invented
/// leaf segment - and a named leaf still works beside it.
#[test]
fn a_self_node_is_the_endpoint_and_named_leaves_sit_beside_it() {
    assert_eq!(runtime::topics().logs().client().key(), "runtime/logs");
    assert_eq!(
        runtime::topics().telemetry().client().key(),
        "runtime/telemetry"
    );
    assert_eq!(
        simulation::topics().clock().client().key(),
        "simulation/clock"
    );

    assert_eq!(
        supervisor::topics().snapshot().client().key(),
        "supervisor/snapshot"
    );
    assert_eq!(
        supervisor::topics().snapshot().current().key(),
        "supervisor/snapshot/current"
    );
    assert_eq!(
        supervisor::topics().connect().client().key(),
        "supervisor/connect"
    );
    assert_eq!(
        supervisor::topics().info().client().key(),
        "supervisor/info"
    );
    assert_eq!(
        supervisor::topics().command().client().key(),
        "supervisor/command"
    );
    assert_eq!(
        supervisor::topics().bundle().get().key(),
        "supervisor/bundle/get"
    );
    assert_eq!(
        supervisor::topics().logs().snapshot().key(),
        "supervisor/logs/snapshot"
    );
    assert_eq!(
        supervisor::topics().logs().follow().key(),
        "supervisor/logs/follow"
    );
    assert_eq!(
        supervisor::topics().telemetry().snapshot().key(),
        "supervisor/telemetry/snapshot"
    );
    assert_eq!(
        supervisor::topics().telemetry().follow().key(),
        "supervisor/telemetry/follow"
    );
}

/// There is one path tree and the side is chosen at the endpoint, so an owner
/// and its clients address a byte-identical key and differ only in the brand
/// the type carries - which the compile-fail fixtures cover.
#[test]
fn the_owner_and_client_sides_render_byte_identical_keys() {
    assert_eq!(
        api::topics().drive().state().owner().key(),
        api::topics().drive().state().client().key()
    );
    assert_eq!(
        api::topics().drive().state().owner().key(),
        "robot/drive/state"
    );

    assert_eq!(
        api::topics().map().submap().owner().key(),
        api::topics().map().submap().client().key()
    );

    let elbow = joint("elbow");
    let joint_node = || {
        api::topics()
            .joint(&elbow)
            .expect("a concrete joint segment")
            .state()
    };
    assert_eq!(
        joint_node().owner().key(),
        joint_node().client().key(),
        "a dynamic path renders the same key on both sides"
    );
    assert_eq!(joint_node().owner().key(), "robot/joint/elbow/state");

    // A self node chooses its side the same way, from the node itself.
    assert_eq!(
        supervisor::topics().snapshot().owner().key(),
        supervisor::topics().snapshot().client().key()
    );
}

/// A dynamic binder narrows its value to one concrete segment. `JointId` is the
/// interesting case: structural names come from authored URDF, whose grammar is
/// wider than a key segment's, so the refusal happens here rather than at the
/// identity.
#[test]
fn a_dynamic_binder_refuses_a_value_that_is_not_one_concrete_segment() {
    for invalid in ["", "a/b", "*", "**", "a\nb"] {
        let id = joint(invalid);
        assert!(
            api::topics().joint(&id).is_err(),
            "{invalid:?} must not cross a dynamic binder"
        );
    }

    let concrete = api::topics()
        .joint(&joint("elbow"))
        .expect("a concrete joint segment")
        .state()
        .client();
    assert!(concrete.publish_key().is_ok());
}
