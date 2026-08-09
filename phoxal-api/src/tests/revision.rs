//! The revision the train selects, and the curated inventory of what it declares.

use phoxal_bus::{ApiVersion, ContractBody, DeliveryFamily, TopicRole};

use crate::{RobotApi, v0_1 as api};

#[test]
fn v0_2_is_the_train_selected_revision_while_v0_1_remains_immutable() {
    assert_eq!(<api::Api as ApiVersion>::ID, "v0.1");
    assert_eq!(<crate::latest::Api as ApiVersion>::ID, "v0.2");
}

/// The cross-binary API identity and the bus key segment are the same
/// revision wearing two spellings: the metadata record needs a namespaced
/// identity that is unambiguous next to four other version identities, while
/// the key segment is addressing inside an already-Phoxal keyspace and stays
/// bare. This is the only place the two are tied together.
#[test]
fn the_declared_api_identity_namespaces_the_train_selected_revision() {
    assert_eq!(
        RobotApi::V0_2.as_str(),
        format!(
            "phoxal/robot-api/{}",
            <crate::latest::Api as ApiVersion>::ID
        )
    );
}

/// Every command topic is classified as one-shot, leased, or internal
/// actuation in the inventory below. Stream topics have their own semantic
/// family and are checked separately.
///
/// This pins the written classification to the api tree itself: adding a
/// command topic without deciding what kind of command it is fails here, which
/// is the only way the inventory stays true.
#[test]
fn every_command_topic_is_classified() {
    /// The canonical command classification. Keep it exhaustive.
    const CLASSIFIED: &[(&str, &str)] = &[
        // Leased: a continuous authority a live sender must keep renewing.
        ("v0.1::motion::ManualCommand", "leased"),
        // Internal actuation: produced by an on-robot participant inside the
        // control chain, and expiring through the receiver's own deadline.
        ("v0.1::drive::Target", "internal actuation"),
        ("v0.1::component::motor::Command", "internal actuation"),
        // One-shot: a single request that either takes effect or does not, and
        // that nothing has to keep repeating.
        ("v0.1::power::Command", "one-shot"),
        ("v0.1::navigation::Request", "one-shot"),
        ("v0.1::component::led::Command", "one-shot"),
        ("v0.1::component::speaker::Chunk", "one-shot"),
        ("v0.2::motion::ManualCommand", "leased"),
        ("v0.2::drive::Target", "internal actuation"),
        ("v0.2::component::motor::Command", "internal actuation"),
        ("v0.2::component::led::Command", "one-shot"),
    ];

    let declared: std::collections::BTreeSet<&str> = crate::API_CONTRACT_MANIFEST
        .iter()
        .flat_map(|version| version.contracts.iter())
        .filter(|contract| contract.role == TopicRole::Command)
        .map(|contract| contract.family)
        .collect();
    let classified: std::collections::BTreeSet<&str> =
        CLASSIFIED.iter().map(|(family, _)| *family).collect();

    assert_eq!(
        declared, classified,
        "every command topic must have a classification"
    );
}

#[test]
fn every_stream_topic_is_classified() {
    let declared: std::collections::BTreeSet<&str> = crate::API_CONTRACT_MANIFEST
        .iter()
        .flat_map(|version| version.contracts.iter())
        .filter(|contract| contract.role == TopicRole::Stream)
        .map(|contract| contract.family)
        .collect();

    let classified = ["v0.2::component::speaker::Chunk"]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        declared, classified,
        "every stream topic must be classified"
    );
}

#[test]
fn speaker_chunk_changes_from_command_to_stream_only_in_v0_2() {
    type V1 = crate::v0_1::component::speaker::Chunk;
    type V2 = crate::v0_2::component::speaker::Chunk;
    assert_eq!(<V1 as ContractBody>::ROLE, TopicRole::Command);
    assert_eq!(<V1 as ContractBody>::DELIVERY, DeliveryFamily::Setpoint);
    assert_eq!(<V2 as ContractBody>::ROLE, TopicRole::Stream);
    assert_eq!(<V2 as ContractBody>::DELIVERY, DeliveryFamily::Stream);
}

#[test]
fn navigation_result_is_an_owner_event_with_ordered_delivery_only_in_v0_2() {
    type V1 = crate::v0_1::navigation::Result;
    type V2 = crate::v0_2::navigation::Result;
    assert_eq!(<V1 as ContractBody>::ROLE, TopicRole::State);
    assert_eq!(<V1 as ContractBody>::DELIVERY, DeliveryFamily::State);
    assert_eq!(<V2 as ContractBody>::ROLE, TopicRole::State);
    assert_eq!(<V2 as ContractBody>::DELIVERY, DeliveryFamily::Stream);
}

#[test]
fn generated_contract_manifest_lists_contract_shapes() {
    assert_eq!(
        crate::API_CONTRACT_MANIFEST.len(),
        2,
        "the train ships the immutable v0.1 and current v0.2 revisions"
    );

    let version = crate::API_CONTRACT_MANIFEST
        .iter()
        .find(|version| version.name == "v0.1")
        .expect("v0.1 should be in the generated manifest");

    let drive_state = version
        .contracts
        .iter()
        .find(|contract| contract.family == "v0.1::drive::State")
        .expect("drive::State should be in the generated manifest");
    assert_eq!(drive_state.topic, "v0.1/drive/state");

    // A contract under two dynamic nodes carries both placeholders in the key
    // the manifest reports, exactly as `ContractBody::TOPIC` does.
    let battery_state = version
        .contracts
        .iter()
        .find(|contract| contract.family == "v0.1::component::battery::State")
        .expect("component::battery::State should be in the v0.1 manifest entry");
    assert_eq!(
        battery_state.topic,
        "v0.1/component/{instance}/battery/{capability}/state"
    );

    let current = crate::API_CONTRACT_MANIFEST
        .iter()
        .find(|version| version.name == "v0.2")
        .expect("v0.2 should be in the generated manifest");
    let current_drive_state = current
        .contracts
        .iter()
        .find(|contract| contract.family == "v0.2::drive::State")
        .expect("drive::State should be in the v0.2 manifest");
    assert_eq!(current_drive_state.topic, "v0.2/drive/state");
}
