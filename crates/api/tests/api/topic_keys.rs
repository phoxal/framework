//! Endpoint key composition: how a node's path in the tree becomes a
//! wire key.

use phoxal_bus::EndpointDescriptor;

use crate::robot as api;

/// The contract family is the leading key segment, so a leaf name reused in
/// another family cannot collide with this one.
#[test]
fn endpoint_topic_is_rooted_at_its_family() {
    assert_eq!(
        <api::endpoint::drive::StateEndpoint as EndpointDescriptor>::TOPIC,
        "robot/drive/state"
    );
    assert_eq!(
        <api::endpoint::drive::TargetEndpoint as EndpointDescriptor>::TOPIC,
        "robot/drive/target"
    );
    assert_eq!(
        <api::endpoint::navigation::StateEndpoint as EndpointDescriptor>::TOPIC,
        "robot/navigation/state"
    );
    assert_eq!(
        <api::endpoint::joint::StateEndpoint as EndpointDescriptor>::TOPIC,
        "robot/joint/{joint}/state"
    );
    assert_eq!(
        <api::endpoint::video::OpenEndpoint as EndpointDescriptor>::TOPIC,
        "robot/video/open"
    );
    assert_eq!(
        <api::endpoint::localize::StateEndpoint as EndpointDescriptor>::TOPIC,
        "robot/localize/state"
    );
}

/// A dynamic node contributes a `{var}` placeholder to the documented key, at
/// the position it occupies in the node path.
#[test]
fn dynamic_endpoint_topic_is_derived_from_node_path() {
    assert_eq!(
        <api::endpoint::component::motor::CommandEndpoint as EndpointDescriptor>::TOPIC,
        "robot/component/{instance}/motor/{capability}/command"
    );
    assert_eq!(
        <api::endpoint::component::imu::SampleEndpoint as EndpointDescriptor>::TOPIC,
        "robot/component/{instance}/imu/{capability}/sample"
    );
    assert_eq!(
        <api::endpoint::component::camera::FrameEndpoint as EndpointDescriptor>::TOPIC,
        "robot/component/{instance}/camera/{capability}/frame"
    );
    assert_eq!(
        <api::endpoint::component::encoder::SampleEndpoint as EndpointDescriptor>::TOPIC,
        "robot/component/{instance}/encoder/{capability}/sample"
    );
}

#[test]
fn current_control_contracts_use_the_robot_family_keyspace() {
    assert_eq!(
        <crate::robot::endpoint::drive::TargetEndpoint as EndpointDescriptor>::TOPIC,
        "robot/drive/target"
    );
    assert_eq!(
        <crate::robot::endpoint::drive::StateEndpoint as EndpointDescriptor>::TOPIC,
        "robot/drive/state"
    );
    assert_eq!(
        <crate::robot::endpoint::motion::ManualEndpoint as EndpointDescriptor>::TOPIC,
        "robot/motion/manual"
    );
    assert_eq!(
        <crate::robot::endpoint::motion::StateEndpoint as EndpointDescriptor>::TOPIC,
        "robot/motion/state"
    );
}
