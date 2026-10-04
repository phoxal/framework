//! Production contracts checked against independent, test-owned protoc inputs.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use phoxal::communication::{bootstrap, execution, session, simulation};
use prost::{Message, Name};
use prost_reflect::{DescriptorPool, DynamicMessage, Kind, MessageDescriptor, Value};

mod reference {
    pub mod google {
        include!(concat!(env!("OUT_DIR"), "/google.protobuf.rs"));
    }
    pub mod bootstrap {
        include!(concat!(env!("OUT_DIR"), "/phoxal.bootstrap.v1.rs"));
    }
    pub mod session {
        include!(concat!(env!("OUT_DIR"), "/phoxal.session.v1.rs"));
    }
    pub mod execution {
        include!(concat!(env!("OUT_DIR"), "/phoxal.execution.v1.rs"));
    }
    pub mod simulation {
        include!(concat!(env!("OUT_DIR"), "/phoxal.simulation.v1.rs"));
    }
}

#[test]
fn standard_empty_uses_the_canonical_message_path() {
    exchange::<phoxal::contracts::Empty, reference::google::Empty>();
    assert_eq!(
        phoxal::contracts::Empty::full_name(),
        "google.protobuf.Empty"
    );
    assert_eq!(
        phoxal::contracts::Empty::decode(&[0x08, 0x01][..]).unwrap(),
        phoxal::contracts::Empty {}
    );
    assert_eq!(
        reference::google::Empty::decode(&[0x08, 0x01][..]).unwrap(),
        reference::google::Empty {}
    );
}

fn reference_pool() -> DescriptorPool {
    DescriptorPool::decode(include_bytes!(concat!(env!("OUT_DIR"), "/reference.bin")).as_slice())
        .expect("independently compiled descriptors")
}

fn populated(descriptor: MessageDescriptor) -> DynamicMessage {
    let mut message = DynamicMessage::new(descriptor.clone());
    for field in descriptor.fields() {
        let value = match field.kind() {
            Kind::String => Value::String(format!("field-{}", field.number())),
            Kind::Bytes => Value::Bytes(vec![0, 1, 255].into()),
            Kind::Uint32 => Value::U32(17),
            Kind::Uint64 => Value::U64(u64::MAX),
            Kind::Int32 => Value::I32(-7),
            Kind::Bool => Value::Bool(true),
            Kind::Enum(enumeration) => {
                Value::EnumNumber(enumeration.values().last().unwrap().number())
            }
            Kind::Message(nested) => Value::Message(populated(nested)),
            other => panic!("unexpected protocol kind {other:?}"),
        };
        message.set_field(
            &field,
            if field.is_list() {
                Value::List(vec![value.clone(), value])
            } else {
                value
            },
        );
    }
    message
}

fn exchange<T: Message + Name + Default, R: Message + Default>() {
    let pool = reference_pool();
    let descriptor = pool
        .get_message_by_name(&T::full_name())
        .expect("reference message");
    let reference = populated(descriptor.clone());
    let compiled = R::decode(reference.encode_to_vec().as_slice())
        .expect("protoc codec accepts populated reference");
    let authored =
        T::decode(compiled.encode_to_vec().as_slice()).expect("reference bytes accepted");
    let decoded = DynamicMessage::decode(descriptor, authored.encode_to_vec().as_slice())
        .expect("authored bytes accepted independently");
    assert_eq!(decoded, reference, "{}", T::full_name());
}

#[test]
fn every_production_descriptor_agrees_with_protoc() {
    let authored = DescriptorPool::decode(
        phoxal::communication::file_descriptor_set()
            .unwrap()
            .encode_to_vec()
            .as_slice(),
    )
    .unwrap();
    let reference = reference_pool();
    for message in authored.all_messages() {
        let other = reference
            .get_message_by_name(message.full_name())
            .expect("reference owns this message");
        assert_eq!(
            message.fields().count(),
            other.fields().count(),
            "{}",
            message.full_name()
        );
        for field in message.fields() {
            let other = other.get_field(field.number()).expect("stable tag");
            assert_eq!(field.name(), other.name());
            assert_eq!(format!("{:?}", field.kind()), format!("{:?}", other.kind()));
            assert_eq!(field.is_list(), other.is_list());
            assert_eq!(
                field.supports_presence(),
                other.supports_presence(),
                "{}.{}",
                message.full_name(),
                field.name()
            );
            match (field.default_value(), other.default_value()) {
                (Value::Message(a), Value::Message(b)) => {
                    assert_eq!(a.encode_to_vec(), b.encode_to_vec())
                }
                (a, b) => assert_eq!(a, b),
            }
        }
    }
    for enumeration in authored.all_enums() {
        let other = reference.get_enum_by_name(enumeration.full_name()).unwrap();
        let values = |e: prost_reflect::EnumDescriptor| {
            e.values()
                .map(|v| (v.name().to_owned(), v.number()))
                .collect::<Vec<_>>()
        };
        assert_eq!(values(enumeration), values(other));
    }
    let expected = reference
        .all_messages()
        .filter(|m| {
            [
                "phoxal.bootstrap.v1",
                "phoxal.session.v1",
                "phoxal.execution.v1",
                "phoxal.simulation.v1",
            ]
            .contains(&m.package_name())
        })
        .count();
    assert_eq!(
        authored.all_messages().count(),
        expected,
        "descriptor inventory is complete"
    );
}

#[test]
fn scalar_and_nested_presence_match_the_independent_codec() {
    let authored = session::BindMethodRequest {
        expected: Some(session::MethodMetadata {
            lease_valid_for_ms: Some(0),
            ..Default::default()
        }),
        ..Default::default()
    };
    let reference =
        reference::session::BindMethodRequest::decode(authored.encode_to_vec().as_slice()).unwrap();
    assert_eq!(
        reference.expected.as_ref().unwrap().lease_valid_for_ms,
        Some(0)
    );
    assert_eq!(authored.encode_to_vec(), reference.encode_to_vec());
    let absent = session::BindMethodRequest::default();
    assert!(
        reference::session::BindMethodRequest::decode(absent.encode_to_vec().as_slice())
            .unwrap()
            .expected
            .is_none()
    );
    let present_empty = simulation::Observation {
        membership: Some(simulation::ProductMembership::default()),
        payload: vec![],
    };
    let reference =
        reference::simulation::Observation::decode(present_empty.encode_to_vec().as_slice())
            .unwrap();
    assert!(reference.membership.is_some());
    assert_eq!(present_empty.encode_to_vec(), reference.encode_to_vec());
    let detail = session::SupervisorStatusResponse {
        detail: Some(String::new()),
        ..Default::default()
    };
    let reference =
        reference::session::SupervisorStatusResponse::decode(detail.encode_to_vec().as_slice())
            .unwrap();
    assert_eq!(reference.detail, Some(String::new()));
    assert_eq!(detail.state, session::SupervisorState::Unspecified);
    assert_eq!(detail.encode_to_vec(), reference.encode_to_vec());
}

#[test]
fn typed_protocol_enums_refuse_unknown_numbers_instead_of_defaulting() {
    macro_rules! rejects {
        ($authored:ty, $reference:path, $field:ident) => {
            for number in [99, -1] {
                use $reference as Independent;
                let reference = Independent {
                    $field: number,
                    ..Default::default()
                };
                let bytes = reference.encode_to_vec();
                assert_eq!(
                    <$reference>::decode(bytes.as_slice()).unwrap().$field,
                    number
                );
                assert!(
                    <$authored>::decode(bytes.as_slice())
                        .unwrap_err()
                        .to_string()
                        .contains("unknown enumeration")
                );
            }
        };
    }
    rejects!(
        session::SupervisorStatusResponse,
        reference::session::SupervisorStatusResponse,
        state
    );
    rejects!(
        session::ExecutionSummary,
        reference::session::ExecutionSummary,
        state
    );
    rejects!(
        session::MethodMetadata,
        reference::session::MethodMetadata,
        shape
    );
    rejects!(
        session::OperationResponse,
        reference::session::OperationResponse,
        outcome
    );
    rejects!(
        session::SubscriptionRecord,
        reference::session::SubscriptionRecord,
        kind
    );
    rejects!(
        execution::AdmitExecutionRequest,
        reference::execution::AdmitExecutionRequest,
        mode
    );
    rejects!(
        simulation::ProductMembership,
        reference::simulation::ProductMembership,
        disposition
    );
    rejects!(
        simulation::CutReceipt,
        reference::simulation::CutReceipt,
        status
    );
    let nested = reference::simulation::Observation {
        membership: Some(reference::simulation::ProductMembership {
            disposition: 99,
            ..Default::default()
        }),
        payload: vec![],
    };
    assert!(simulation::Observation::decode(nested.encode_to_vec().as_slice()).is_err());
    // Unknown occurrences cannot be hidden by a subsequent known value.
    let overwritten = &[0x28, 99, 0x28, 1][..];
    assert_eq!(
        reference::execution::AdmitExecutionRequest::decode(overwritten)
            .unwrap()
            .mode,
        1
    );
    assert!(execution::AdmitExecutionRequest::decode(overwritten).is_err());
}

#[test]
fn malformed_values_refuse_and_unknown_fields_remain_skippable() {
    for bytes in [
        &[0x0a, 0x01, 0xff][..],
        &[0x0a, 0x02, 0x01][..],
        &[0x08, 0x01][..],
    ] {
        assert!(session::OpenSessionRequest::decode(bytes).is_err());
        assert!(reference::session::OpenSessionRequest::decode(bytes).is_err());
    }
    // Unknown ordinary fields remain skippable, including unknown fields inside nested messages.
    let unknown = &[0xa0, 0x06, 0x01][..];
    assert_eq!(
        execution::Invocation::decode(unknown).unwrap(),
        execution::Invocation::default()
    );
    assert_eq!(
        reference::execution::Invocation::decode(unknown).unwrap(),
        reference::execution::Invocation::default()
    );
    let nested = &[0x0a, 0x03, 0xa0, 0x06, 0x01][..];
    assert!(
        simulation::Observation::decode(nested)
            .unwrap()
            .membership
            .is_some()
    );
    assert!(
        reference::simulation::Observation::decode(nested)
            .unwrap()
            .membership
            .is_some()
    );
    assert!(simulation::Observation::decode(&[0x0a, 0x02, 0x08][..]).is_err());
    assert!(reference::simulation::Observation::decode(&[0x0a, 0x02, 0x08][..]).is_err());
}

#[test]
fn all_production_messages_exchange_non_default_and_repeated_values() {
    exchange::<bootstrap::SessionOffers, reference::bootstrap::SessionOffers>();
    exchange::<bootstrap::SessionOffer, reference::bootstrap::SessionOffer>();
    exchange::<session::OpenSessionRequest, reference::session::OpenSessionRequest>();
    exchange::<session::OpenSessionResponse, reference::session::OpenSessionResponse>();
    exchange::<session::RenewSessionRequest, reference::session::RenewSessionRequest>();
    exchange::<session::RenewSessionResponse, reference::session::RenewSessionResponse>();
    exchange::<session::CloseSessionRequest, reference::session::CloseSessionRequest>();
    exchange::<session::CloseSessionResponse, reference::session::CloseSessionResponse>();
    exchange::<session::SupervisorInfoRequest, reference::session::SupervisorInfoRequest>();
    exchange::<session::SupervisorInfoResponse, reference::session::SupervisorInfoResponse>();
    exchange::<session::SupervisorStatusRequest, reference::session::SupervisorStatusRequest>();
    exchange::<session::SupervisorStatusResponse, reference::session::SupervisorStatusResponse>();
    exchange::<session::ListExecutionsRequest, reference::session::ListExecutionsRequest>();
    exchange::<session::ListExecutionsResponse, reference::session::ListExecutionsResponse>();
    exchange::<session::ExecutionSummary, reference::session::ExecutionSummary>();
    exchange::<session::ListMethodsRequest, reference::session::ListMethodsRequest>();
    exchange::<session::ListMethodsResponse, reference::session::ListMethodsResponse>();
    exchange::<session::MethodMetadata, reference::session::MethodMetadata>();
    exchange::<session::BindMethodRequest, reference::session::BindMethodRequest>();
    exchange::<session::BindMethodResponse, reference::session::BindMethodResponse>();
    exchange::<session::OperationRequest, reference::session::OperationRequest>();
    exchange::<session::OperationResponse, reference::session::OperationResponse>();
    exchange::<session::SubscriptionRequest, reference::session::SubscriptionRequest>();
    exchange::<session::SubscriptionAdmission, reference::session::SubscriptionAdmission>();
    exchange::<session::SubscriptionRecord, reference::session::SubscriptionRecord>();
    exchange::<execution::ContractRequirement, reference::execution::ContractRequirement>();
    exchange::<execution::AdmitExecutionRequest, reference::execution::AdmitExecutionRequest>();
    exchange::<execution::AdmitExecutionResponse, reference::execution::AdmitExecutionResponse>();
    exchange::<execution::Ready, reference::execution::Ready>();
    exchange::<execution::InitializeStateRequest, reference::execution::InitializeStateRequest>();
    exchange::<execution::InitializeStateResponse, reference::execution::InitializeStateResponse>();
    exchange::<execution::PinReadViewsRequest, reference::execution::PinReadViewsRequest>();
    exchange::<execution::PinReadViewsResponse, reference::execution::PinReadViewsResponse>();
    exchange::<execution::Invocation, reference::execution::Invocation>();
    exchange::<execution::InvocationAccepted, reference::execution::InvocationAccepted>();
    exchange::<execution::ProductReceipt, reference::execution::ProductReceipt>();
    exchange::<execution::InputReceipt, reference::execution::InputReceipt>();
    exchange::<execution::Actuation, reference::execution::Actuation>();
    exchange::<execution::DeliveryReceipt, reference::execution::DeliveryReceipt>();
    exchange::<execution::DeliveryAck, reference::execution::DeliveryAck>();
    exchange::<execution::ResetExecutionRequest, reference::execution::ResetExecutionRequest>();
    exchange::<execution::ResetExecutionResponse, reference::execution::ResetExecutionResponse>();
    exchange::<execution::RuntimeFailure, reference::execution::RuntimeFailure>();
    exchange::<execution::RuntimeWireMetadata, reference::execution::RuntimeWireMetadata>();
    exchange::<simulation::AcquireAuthorityRequest, reference::simulation::AcquireAuthorityRequest>(
    );
    exchange::<simulation::ProviderRequirement, reference::simulation::ProviderRequirement>();
    exchange::<simulation::AcquireAuthorityResponse, reference::simulation::AcquireAuthorityResponse>(
    );
    exchange::<simulation::TransitionKey, reference::simulation::TransitionKey>();
    exchange::<simulation::ProductMembership, reference::simulation::ProductMembership>();
    exchange::<simulation::Observation, reference::simulation::Observation>();
    exchange::<simulation::Actuation, reference::simulation::Actuation>();
    exchange::<simulation::ProductReceipt, reference::simulation::ProductReceipt>();
    exchange::<simulation::CutReceipt, reference::simulation::CutReceipt>();
    exchange::<
        simulation::AdmitInitialObservationsRequest,
        reference::simulation::AdmitInitialObservationsRequest,
    >();
    exchange::<
        simulation::AdmitInitialObservationsResponse,
        reference::simulation::AdmitInitialObservationsResponse,
    >();
    exchange::<simulation::PrepareBoundaryRequest, reference::simulation::PrepareBoundaryRequest>();
    exchange::<simulation::PrepareBoundaryResponse, reference::simulation::PrepareBoundaryResponse>(
    );
    exchange::<simulation::AdmitObservationsRequest, reference::simulation::AdmitObservationsRequest>(
    );
    exchange::<
        simulation::AdmitObservationsResponse,
        reference::simulation::AdmitObservationsResponse,
    >();
    exchange::<simulation::ResetRequest, reference::simulation::ResetRequest>();
    exchange::<simulation::ResetResponse, reference::simulation::ResetResponse>();
    exchange::<simulation::ReleaseAuthorityRequest, reference::simulation::ReleaseAuthorityRequest>(
    );
    exchange::<simulation::ReleaseAuthorityResponse, reference::simulation::ReleaseAuthorityResponse>(
    );
    exchange::<simulation::ProgressRequest, reference::simulation::ProgressRequest>();
    exchange::<simulation::ProgressResponse, reference::simulation::ProgressResponse>();
}

#[test]
fn prepared_apis_reference_the_canonical_protocol_owner() {
    let pool = DescriptorPool::from_file_descriptor_set(
        phoxal::communication::file_descriptor_set().unwrap(),
    )
    .unwrap();
    for name in pool
        .all_messages()
        .map(|message| message.full_name().to_owned())
        .chain(
            pool.all_enums()
                .map(|enumeration| enumeration.full_name().to_owned()),
        )
    {
        let path =
            phoxal_build::sdk_type_path(&name).expect("each SDK protocol has a canonical path");
        let parts = name.split('.').collect::<Vec<_>>();
        assert_eq!(
            path,
            format!("::phoxal::communication::{}::{}", parts[1], parts[3])
        );
    }
}
