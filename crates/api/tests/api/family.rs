//! The contract families this crate declares, and the catalogue they emit.

use phoxal_bus::{ApiFamily, EndpointDescriptor, EndpointKind};

use crate::robot as api;

#[test]
fn every_family_identifies_itself_by_name() {
    assert_eq!(<api::Api as ApiFamily>::ID, "robot");
    assert_eq!(<crate::runtime::Api as ApiFamily>::ID, "runtime");
    assert_eq!(<crate::supervisor::Api as ApiFamily>::ID, "supervisor");
    assert_eq!(
        <api::endpoint::drive::StateEndpoint as EndpointDescriptor>::FAMILY,
        "robot",
    );
    assert_eq!(
        <crate::runtime::endpoint::logs::TopicEndpoint as EndpointDescriptor>::FAMILY,
        "runtime",
    );
    assert_eq!(
        <crate::supervisor::endpoint::snapshot::TopicEndpoint as EndpointDescriptor>::FAMILY,
        "supervisor",
    );
}

#[test]
fn every_command_endpoint_is_classified() {
    const CLASSIFIED: &[&str] = &[
        "robot::motion::ManualEndpoint",
        "robot::drive::TargetEndpoint",
        "robot::component::motor::CommandEndpoint",
        "robot::component::led::CommandEndpoint",
    ];

    let declared = family("robot")
        .contracts
        .iter()
        .filter(|contract| contract.kind == EndpointKind::Setpoint)
        .map(|contract| contract.endpoint)
        .collect::<std::collections::BTreeSet<_>>();
    let classified = CLASSIFIED.iter().copied().collect();
    assert_eq!(declared, classified);
}

#[test]
fn stream_and_event_semantics_are_materialized_directly() {
    type Speaker = api::endpoint::component::speaker::StreamEndpoint;
    type NavigationResult = api::endpoint::navigation::ResultEndpoint;
    assert_eq!(<Speaker as EndpointDescriptor>::KIND, EndpointKind::Stream);
    assert_eq!(
        <NavigationResult as EndpointDescriptor>::KIND,
        EndpointKind::Event,
    );
}

fn family(name: &str) -> &'static crate::ApiContractManifestFamily {
    crate::API_CONTRACT_MANIFEST
        .iter()
        .find(|family| family.name == name)
        .expect("declared family")
}

#[test]
fn the_generated_catalogue_holds_every_family_rooted_at_its_own_name() {
    let names = crate::API_CONTRACT_MANIFEST
        .iter()
        .map(|family| family.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["robot", "runtime", "supervisor"]);
    for family in crate::API_CONTRACT_MANIFEST {
        for contract in family.contracts {
            assert!(
                contract.topic == family.name
                    || contract.topic.starts_with(&format!("{}/", family.name)),
                "{} is not rooted at the {} family",
                contract.topic,
                family.name
            );
        }
    }

    let family = family("robot");
    let drive_state = family
        .contracts
        .iter()
        .find(|contract| contract.endpoint == "robot::drive::StateEndpoint")
        .expect("drive state endpoint");
    assert_eq!(drive_state.topic, "robot/drive/state");
    assert_eq!(drive_state.kind, EndpointKind::State);
    assert!(
        drive_state
            .payload
            .is_some_and(|path| path.ends_with("::robot::drive::State"))
    );

    let battery_state = family
        .contracts
        .iter()
        .find(|contract| contract.endpoint == "robot::component::battery::StateEndpoint")
        .expect("battery state endpoint");
    assert_eq!(
        battery_state.topic,
        "robot/component/{instance}/battery/{capability}/state",
    );
}
