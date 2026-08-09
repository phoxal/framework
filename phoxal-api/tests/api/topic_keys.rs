//! Endpoint key composition: how a node's path in the tree becomes a
//! wire key.

use phoxal_bus::EndpointDescriptor;

use crate::v0_1 as api;

/// The API version is folded into the wire key, so two participants using the
/// same version-qualified contract share a key that cannot collide with any
/// other version's contract of the same leaf name.
#[test]
fn endpoint_topic_is_version_qualified() {
    assert_eq!(
        <api::endpoint::drive::StateEndpoint as EndpointDescriptor>::TOPIC,
        "v0.1/drive/state"
    );
    assert_eq!(
        <api::endpoint::drive::TargetEndpoint as EndpointDescriptor>::TOPIC,
        "v0.1/drive/target"
    );
    assert_eq!(
        <api::endpoint::navigation::StateEndpoint as EndpointDescriptor>::TOPIC,
        "v0.1/navigation/state"
    );
    assert_eq!(
        <api::endpoint::joint::StateEndpoint as EndpointDescriptor>::TOPIC,
        "v0.1/joint/{joint}/state"
    );
    assert_eq!(
        <api::endpoint::video::OpenEndpoint as EndpointDescriptor>::TOPIC,
        "v0.1/video/open"
    );
    assert_eq!(
        <api::endpoint::localize::StateEndpoint as EndpointDescriptor>::TOPIC,
        "v0.1/localize/state"
    );
}

/// A dynamic node contributes a `{var}` placeholder to the documented key, at
/// the position it occupies in the node path.
#[test]
fn dynamic_endpoint_topic_is_derived_from_node_path() {
    assert_eq!(
        <api::endpoint::component::motor::CommandEndpoint as EndpointDescriptor>::TOPIC,
        "v0.1/component/{instance}/motor/{capability}/command"
    );
    assert_eq!(
        <api::endpoint::component::imu::SampleEndpoint as EndpointDescriptor>::TOPIC,
        "v0.1/component/{instance}/imu/{capability}/sample"
    );
    assert_eq!(
        <api::endpoint::component::camera::FrameEndpoint as EndpointDescriptor>::TOPIC,
        "v0.1/component/{instance}/camera/{capability}/frame"
    );
    assert_eq!(
        <api::endpoint::component::encoder::SampleEndpoint as EndpointDescriptor>::TOPIC,
        "v0.1/component/{instance}/encoder/{capability}/sample"
    );
}

#[test]
fn current_control_contracts_use_the_v0_1_keyspace() {
    assert_eq!(
        <crate::v0_1::endpoint::drive::TargetEndpoint as EndpointDescriptor>::TOPIC,
        "v0.1/drive/target"
    );
    assert_eq!(
        <crate::v0_1::endpoint::drive::StateEndpoint as EndpointDescriptor>::TOPIC,
        "v0.1/drive/state"
    );
    assert_eq!(
        <crate::v0_1::endpoint::motion::ManualEndpoint as EndpointDescriptor>::TOPIC,
        "v0.1/motion/manual"
    );
    assert_eq!(
        <crate::v0_1::endpoint::motion::StateEndpoint as EndpointDescriptor>::TOPIC,
        "v0.1/motion/state"
    );
}
