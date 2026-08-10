//! The sole pre-v1 revision selected by this release train.

use phoxal_bus::{ApiVersion, EndpointDescriptor, EndpointKind};
use phoxal_runtime_contract::version::RobotApiVersion;

use crate::{RobotApi, v0_1 as api};

#[test]
fn v0_1_is_the_only_known_and_train_selected_revision() {
    assert_eq!(<api::Api as ApiVersion>::ID, "v0.1");
    assert_eq!(<crate::latest::Api as ApiVersion>::ID, "v0.1");
    assert_eq!(RobotApi::ALL, &[RobotApi::V0_1]);
    assert_eq!(RobotApi::LATEST, RobotApi::V0_1);
    assert_eq!(RobotApi::V0_1.as_str(), "phoxal/robot-api/v0.1",);
    assert_eq!(RobotApi::V0_1.version(), RobotApiVersion::new(0, 1));
    assert_eq!(
        RobotApi::from_version(RobotApiVersion::new(0, 1)),
        Some(RobotApi::V0_1),
    );
}

#[test]
fn an_unknown_valid_version_reports_that_the_robot_api_is_unsupported() {
    let advertised: RobotApiVersion =
        serde_json::from_str("\"phoxal/robot-api/v42.7\"").expect("valid open Robot API");
    let error = RobotApi::try_from(advertised).expect_err("this train has no matching adapter");
    assert_eq!(error.version(), advertised);
    assert_eq!(
        error.to_string(),
        "unsupported Robot API `phoxal/robot-api/v42.7`",
    );
}

#[test]
fn every_command_endpoint_is_classified() {
    const CLASSIFIED: &[&str] = &[
        "v0.1::motion::ManualEndpoint",
        "v0.1::drive::TargetEndpoint",
        "v0.1::component::motor::CommandEndpoint",
        "v0.1::component::led::CommandEndpoint",
    ];

    let declared = crate::API_CONTRACT_MANIFEST[0]
        .contracts
        .iter()
        .filter(|contract| contract.kind == EndpointKind::Setpoint)
        .map(|contract| contract.endpoint)
        .collect::<std::collections::BTreeSet<_>>();
    let classified = CLASSIFIED.iter().copied().collect();
    assert_eq!(declared, classified);
}

#[test]
fn stream_and_event_semantics_are_materialized_directly_in_v0_1() {
    type Speaker = api::endpoint::component::speaker::StreamEndpoint;
    type NavigationResult = api::endpoint::navigation::ResultEndpoint;
    assert_eq!(<Speaker as EndpointDescriptor>::KIND, EndpointKind::Stream);
    assert_eq!(
        <NavigationResult as EndpointDescriptor>::KIND,
        EndpointKind::Event,
    );
}

#[test]
fn generated_endpoint_catalogue_contains_only_v0_1() {
    assert_eq!(crate::API_CONTRACT_MANIFEST.len(), 1);
    let version = &crate::API_CONTRACT_MANIFEST[0];
    assert_eq!(version.name, "v0.1");

    let drive_state = version
        .contracts
        .iter()
        .find(|contract| contract.endpoint == "v0.1::drive::StateEndpoint")
        .expect("drive state endpoint");
    assert_eq!(drive_state.topic, "v0.1/drive/state");
    assert_eq!(drive_state.kind, EndpointKind::State);
    assert!(
        drive_state
            .payload
            .is_some_and(|path| path.ends_with("::v0_1::drive::State"))
    );

    let battery_state = version
        .contracts
        .iter()
        .find(|contract| contract.endpoint == "v0.1::component::battery::StateEndpoint")
        .expect("battery state endpoint");
    assert_eq!(
        battery_state.topic,
        "v0.1/component/{instance}/battery/{capability}/state",
    );
}
