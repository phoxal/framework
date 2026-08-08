//! The api-local topic builders: that a built key equals the contract's
//! documented key, on both sides and through dynamic segments.

use crate::v0_1 as api;

#[test]
fn topic_builder_keys_match_contract_topics() {
    assert_eq!(
        api::topic::client().drive().state().key(),
        "v0.1/drive/state"
    );
    assert_eq!(
        api::topic::client().drive().target().key(),
        "v0.1/drive/target"
    );
    assert_eq!(
        api::topic::client().navigation().state().key(),
        "v0.1/navigation/state"
    );
    assert_eq!(
        api::topic::client().navigation().request().key(),
        "v0.1/navigation/request"
    );
    assert_eq!(
        api::topic::client().navigation().result().key(),
        "v0.1/navigation/result"
    );
    assert_eq!(
        api::topic::client().safety().constraints().key(),
        "v0.1/safety/constraints"
    );
    assert_eq!(
        api::topic::client().safety().state().key(),
        "v0.1/safety/state"
    );
    assert_eq!(api::topic::client().frame().tree().key(), "v0.1/frame/tree");
    assert_eq!(
        api::topic::client().frame().static_transforms().key(),
        "v0.1/frame/static_transforms"
    );
    assert_eq!(
        api::topic::client().power().command().key(),
        "v0.1/power/command"
    );
    assert_eq!(
        api::topic::client().motion().manual().key(),
        "v0.1/motion/manual"
    );
    assert_eq!(
        api::topic::client().simulation().clock().key(),
        "v0.1/simulation/clock"
    );
    assert_eq!(
        api::topic::client().logs("drive").topic().key(),
        "v0.1/logs/drive"
    );
    assert_eq!(
        api::topic::client().supervisor().log().snapshot().key(),
        "v0.1/supervisor/log/snapshot"
    );
    assert_eq!(
        api::topic::client().supervisor().log().follow().key(),
        "v0.1/supervisor/log/follow"
    );
    assert_eq!(
        api::topic::client().supervisor().telemetry().rollup().key(),
        "v0.1/supervisor/telemetry/rollup"
    );
    assert_eq!(
        api::topic::client()
            .supervisor()
            .telemetry()
            .snapshot()
            .key(),
        "v0.1/supervisor/telemetry/snapshot"
    );
    assert_eq!(
        api::topic::client().supervisor().telemetry().follow().key(),
        "v0.1/supervisor/telemetry/follow"
    );
    assert_eq!(
        api::topic::client().perception().detections().key(),
        "v0.1/perception/detections"
    );
    assert_eq!(api::topic::client().video().open().key(), "v0.1/video/open");
    assert_eq!(
        api::topic::client().map().revision().key(),
        "v0.1/map/revision"
    );
    assert_eq!(api::topic::client().map().submap().key(), "v0.1/map/submap");
    assert_eq!(
        api::topic::client().supervisor().asset().get().key(),
        "v0.1/supervisor/asset/get"
    );
    assert_eq!(
        api::topic::client().odometry().state().key(),
        "v0.1/odometry/state"
    );
    assert_eq!(
        api::topic::client().localize().state().key(),
        "v0.1/localize/state"
    );
}

/// The owner builder mirrors the client builder's structure and keys exactly;
/// only the leaf brand differs, which is a compile-time property covered by the
/// trybuild fixtures rather than anything observable at runtime. What is
/// checkable here is that the two sides address the same key, so an owner and
/// its clients cannot end up on different topics.
#[test]
fn owner_builder_produces_identical_keys() {
    assert_eq!(
        api::topic::owner().drive().state().key(),
        "v0.1/drive/state"
    );
    assert_eq!(
        api::topic::owner().drive().target().key(),
        "v0.1/drive/target"
    );
    assert_eq!(
        api::topic::owner().supervisor().log().snapshot().key(),
        "v0.1/supervisor/log/snapshot"
    );
    assert_eq!(
        api::topic::owner().supervisor().telemetry().rollup().key(),
        "v0.1/supervisor/telemetry/rollup"
    );
    assert_eq!(api::topic::owner().map().submap().key(), "v0.1/map/submap");
    assert_eq!(api::topic::owner().video().open().key(), "v0.1/video/open");
    assert_eq!(
        api::topic::owner().logs("drive").topic().key(),
        "v0.1/logs/drive"
    );
    // Dynamic node path: the owner builder fills carried vars the same way.
    assert_eq!(
        api::topic::owner().joint("elbow").state().key(),
        "v0.1/joint/elbow/state"
    );
}

#[test]
fn dynamic_topic_builder_fills_the_key_from_node_vars() {
    assert_eq!(
        api::topic::client().joint("elbow").state().key(),
        "v0.1/joint/elbow/state"
    );
    let topic = api::topic::client()
        .component("front_left_drive")
        .motor("motor")
        .command();
    assert_eq!(
        topic.key(),
        "v0.1/component/front_left_drive/motor/motor/command"
    );
    let enc = api::topic::client()
        .component("front_left_drive")
        .encoder("encoder")
        .sample();
    assert_eq!(
        enc.key(),
        "v0.1/component/front_left_drive/encoder/encoder/sample"
    );
}

#[test]
fn component_capability_topic_builders_fill_keys() {
    assert_eq!(
        api::topic::client()
            .component("imu0")
            .accelerometer("accel")
            .sample()
            .key(),
        "v0.1/component/imu0/accelerometer/accel/sample"
    );
    assert_eq!(
        api::topic::client()
            .component("imu0")
            .gyroscope("gyro")
            .sample()
            .key(),
        "v0.1/component/imu0/gyroscope/gyro/sample"
    );
    assert_eq!(
        api::topic::client()
            .component("imu0")
            .magnetometer("mag")
            .sample()
            .key(),
        "v0.1/component/imu0/magnetometer/mag/sample"
    );
    assert_eq!(
        api::topic::client()
            .component("imu0")
            .imu("imu")
            .sample()
            .key(),
        "v0.1/component/imu0/imu/imu/sample"
    );
    assert_eq!(
        api::topic::client()
            .component("base")
            .range("front_tof")
            .sample()
            .key(),
        "v0.1/component/base/range/front_tof/sample"
    );
    assert_eq!(
        api::topic::client()
            .component("gps")
            .gnss("gnss")
            .sample()
            .key(),
        "v0.1/component/gps/gnss/gnss/sample"
    );
    assert_eq!(
        api::topic::client()
            .component("head")
            .camera("front")
            .frame()
            .key(),
        "v0.1/component/head/camera/front/frame"
    );
    assert_eq!(
        api::topic::client()
            .component("head")
            .depth("front_depth")
            .frame()
            .key(),
        "v0.1/component/head/depth/front_depth/frame"
    );
    assert_eq!(
        api::topic::client()
            .component("front_lidar")
            .lidar("scan")
            .scan()
            .key(),
        "v0.1/component/front_lidar/lidar/scan/scan"
    );
    assert_eq!(
        api::topic::client()
            .component("radar")
            .mmwave("mmwave")
            .scan()
            .key(),
        "v0.1/component/radar/mmwave/mmwave/scan"
    );
    assert_eq!(
        api::topic::client()
            .component("head")
            .microphone("mic")
            .frame()
            .key(),
        "v0.1/component/head/microphone/mic/frame"
    );
    assert_eq!(
        api::topic::client()
            .component("status_panel")
            .led("status")
            .command()
            .key(),
        "v0.1/component/status_panel/led/status/command"
    );
    assert_eq!(
        api::topic::client()
            .component("safety_panel")
            .emergency_stop("estop")
            .state()
            .key(),
        "v0.1/component/safety_panel/emergency_stop/estop/state"
    );
}

/// A wildcard fills a dynamic segment for subscription only: it names a set of
/// keys, and publishing to a set is not a thing a publisher can mean.
#[test]
fn dynamic_topic_wildcard_is_subscribe_only() {
    let concrete = api::topic::client()
        .component("base")
        .motor("motor")
        .command();
    assert!(concrete.publish_key().is_ok());

    let wildcard = api::topic::client().component("*").motor("motor").command();
    assert_eq!(wildcard.key(), "v0.1/component/*/motor/motor/command");
    assert!(wildcard.publish_key().is_err());
}
