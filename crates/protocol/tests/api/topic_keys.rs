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

/// A fragment whose whole path is the wire key declares `topic self`, so the
/// key carries no invented leaf segment.
#[test]
fn runtime_family_keys_are_pinned() {
    use phoxal_bus::EndpointKind;

    for (topic, kind) in [
        (
            <crate::runtime::endpoint::logs::TopicEndpoint as EndpointDescriptor>::TOPIC,
            <crate::runtime::endpoint::logs::TopicEndpoint as EndpointDescriptor>::KIND,
        ),
        (
            <crate::runtime::endpoint::telemetry::TopicEndpoint as EndpointDescriptor>::TOPIC,
            <crate::runtime::endpoint::telemetry::TopicEndpoint as EndpointDescriptor>::KIND,
        ),
    ] {
        assert_eq!(kind, EndpointKind::Stream, "{topic}");
    }
    assert_eq!(
        <crate::runtime::endpoint::logs::TopicEndpoint as EndpointDescriptor>::TOPIC,
        "runtime/logs"
    );
    assert_eq!(
        <crate::runtime::endpoint::telemetry::TopicEndpoint as EndpointDescriptor>::TOPIC,
        "runtime/telemetry"
    );
    assert_eq!(
        <crate::runtime::endpoint::simulation::ClockEndpoint as EndpointDescriptor>::TOPIC,
        "runtime/simulation/clock"
    );
    assert_eq!(
        <crate::runtime::endpoint::simulation::ClockEndpoint as EndpointDescriptor>::KIND,
        EndpointKind::Event
    );
}

/// The supervisor family owns supervisor keys only: `runtime` is a sibling
/// family, so the word never appears inside one of these keys.
#[test]
fn supervisor_family_keys_are_pinned() {
    use phoxal_bus::EndpointKind;

    let declared = [
        (
            <crate::supervisor::endpoint::connect::TopicEndpoint as EndpointDescriptor>::TOPIC,
            <crate::supervisor::endpoint::connect::TopicEndpoint as EndpointDescriptor>::KIND,
        ),
        (
            <crate::supervisor::endpoint::snapshot::TopicEndpoint as EndpointDescriptor>::TOPIC,
            <crate::supervisor::endpoint::snapshot::TopicEndpoint as EndpointDescriptor>::KIND,
        ),
        (
            <crate::supervisor::endpoint::snapshot::CurrentEndpoint as EndpointDescriptor>::TOPIC,
            <crate::supervisor::endpoint::snapshot::CurrentEndpoint as EndpointDescriptor>::KIND,
        ),
        (
            <crate::supervisor::endpoint::command::TopicEndpoint as EndpointDescriptor>::TOPIC,
            <crate::supervisor::endpoint::command::TopicEndpoint as EndpointDescriptor>::KIND,
        ),
        (
            <crate::supervisor::endpoint::logs::SnapshotEndpoint as EndpointDescriptor>::TOPIC,
            <crate::supervisor::endpoint::logs::SnapshotEndpoint as EndpointDescriptor>::KIND,
        ),
        (
            <crate::supervisor::endpoint::logs::FollowEndpoint as EndpointDescriptor>::TOPIC,
            <crate::supervisor::endpoint::logs::FollowEndpoint as EndpointDescriptor>::KIND,
        ),
        (
            <crate::supervisor::endpoint::telemetry::SnapshotEndpoint as EndpointDescriptor>::TOPIC,
            <crate::supervisor::endpoint::telemetry::SnapshotEndpoint as EndpointDescriptor>::KIND,
        ),
        (
            <crate::supervisor::endpoint::telemetry::FollowEndpoint as EndpointDescriptor>::TOPIC,
            <crate::supervisor::endpoint::telemetry::FollowEndpoint as EndpointDescriptor>::KIND,
        ),
        (
            <crate::supervisor::endpoint::bundle::GetEndpoint as EndpointDescriptor>::TOPIC,
            <crate::supervisor::endpoint::bundle::GetEndpoint as EndpointDescriptor>::KIND,
        ),
    ];
    assert_eq!(
        declared,
        [
            ("supervisor/connect", EndpointKind::Query),
            ("supervisor/snapshot", EndpointKind::Stream),
            ("supervisor/snapshot/current", EndpointKind::Query),
            ("supervisor/command", EndpointKind::Query),
            ("supervisor/logs/snapshot", EndpointKind::Query),
            ("supervisor/logs/follow", EndpointKind::Stream),
            ("supervisor/telemetry/snapshot", EndpointKind::Query),
            ("supervisor/telemetry/follow", EndpointKind::Stream),
            ("supervisor/bundle/get", EndpointKind::Query),
        ]
    );
    assert!(
        declared.iter().all(|(topic, _)| !topic.contains("runtime")),
        "runtime is a sibling family, not a supervisor key segment"
    );
}

/// The presence lease is a Liveliness key, so it must not collide with any
/// declared endpoint.
#[test]
fn the_presence_lease_collides_with_no_endpoint() {
    assert!(
        crate::API_CONTRACT_MANIFEST
            .iter()
            .flat_map(|family| family.contracts)
            .all(|contract| contract.topic != crate::supervisor::connect::PRESENCE_KEY)
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
