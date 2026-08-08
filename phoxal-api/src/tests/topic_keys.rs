//! `ContractBody::TOPIC` composition: how a node's path in the tree becomes a
//! wire key.

use phoxal_bus::ContractBody;

use crate::v0_1 as api;

/// The API version is folded into the wire key, so two participants using the
/// same version-qualified contract share a key that cannot collide with any
/// other version's contract of the same leaf name.
#[test]
fn contract_body_topic_is_version_qualified() {
    assert_eq!(
        <api::drive::State as ContractBody>::TOPIC,
        "v0.1/drive/state"
    );
    assert_eq!(
        <api::drive::Target as ContractBody>::TOPIC,
        "v0.1/drive/target"
    );
    assert_eq!(
        <api::navigation::State as ContractBody>::TOPIC,
        "v0.1/navigation/state"
    );
    assert_eq!(
        <api::joint::JointState as ContractBody>::TOPIC,
        "v0.1/joint/{joint}/state"
    );
    assert_eq!(
        <api::video::OpenOutcome as ContractBody>::TOPIC,
        "v0.1/video/open"
    );
    assert_eq!(
        <api::localize::LocalizationState as ContractBody>::TOPIC,
        "v0.1/localize/state"
    );
    assert_eq!(
        <api::simulation::Clock as ContractBody>::TOPIC,
        "v0.1/simulation/clock"
    );
    assert_eq!(
        <api::logs::Event as ContractBody>::TOPIC,
        "v0.1/logs/{participant_id}"
    );
    assert_eq!(
        <api::supervisor::log::SnapshotRequest as ContractBody>::TOPIC,
        "v0.1/supervisor/log/snapshot"
    );
    assert_eq!(
        <api::supervisor::log::Snapshot as ContractBody>::TOPIC,
        "v0.1/supervisor/log/snapshot"
    );
    assert_eq!(
        <api::supervisor::log::Follow as ContractBody>::TOPIC,
        "v0.1/supervisor/log/follow"
    );
    assert_eq!(
        <api::supervisor::telemetry::Rollup as ContractBody>::TOPIC,
        "v0.1/supervisor/telemetry/rollup"
    );
    assert_eq!(
        <api::supervisor::telemetry::Snapshot as ContractBody>::TOPIC,
        "v0.1/supervisor/telemetry/snapshot"
    );
    assert_eq!(
        <api::supervisor::telemetry::Follow as ContractBody>::TOPIC,
        "v0.1/supervisor/telemetry/follow"
    );
}

/// A dynamic node contributes a `{var}` placeholder to the documented key, at
/// the position it occupies in the node path.
#[test]
fn dynamic_topic_contract_body_topic_is_derived_from_node_path() {
    assert_eq!(
        <api::component::motor::Command as ContractBody>::TOPIC,
        "v0.1/component/{instance}/motor/{capability}/command"
    );
    assert_eq!(
        <api::component::imu::Sample as ContractBody>::TOPIC,
        "v0.1/component/{instance}/imu/{capability}/sample"
    );
    assert_eq!(
        <api::component::camera::Frame as ContractBody>::TOPIC,
        "v0.1/component/{instance}/camera/{capability}/frame"
    );
    assert_eq!(
        <api::component::encoder::Sample as ContractBody>::TOPIC,
        "v0.1/component/{instance}/encoder/{capability}/sample"
    );
}

#[test]
fn current_control_contracts_use_the_v0_2_keyspace() {
    assert_eq!(
        <crate::v0_2::drive::Target as ContractBody>::TOPIC,
        "v0.2/drive/target"
    );
    assert_eq!(
        <crate::v0_2::drive::State as ContractBody>::TOPIC,
        "v0.2/drive/state"
    );
    assert_eq!(
        <crate::v0_2::motion::ManualCommand as ContractBody>::TOPIC,
        "v0.2/motion/manual"
    );
    assert_eq!(
        <crate::v0_2::motion::State as ContractBody>::TOPIC,
        "v0.2/motion/state"
    );
}
