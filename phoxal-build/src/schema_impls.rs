//! `MessageSchema` and `OneofSchema` implementations for generated client
//! types.
//!
//! Prepared-product code generation emits plain Prost types; these
//! companion implementations give them the same schema, codec, and
//! retention contract as authored types so robot brains can
//! declare runtime endpoints over generated payload types directly.

use std::collections::{BTreeMap, BTreeSet};

use heck::{ToSnakeCase, ToUpperCamelCase};
use prost_reflect::{Cardinality, DescriptorPool, EnumDescriptor, Kind, MessageDescriptor};

/// The SDK-owned standard vocabulary: every wire identity the SDK itself
/// types, mapped to its canonical Rust path. A name under one of these
/// packages that the table omits is not an SDK type; generation emits it
/// locally instead of assuming the SDK provides it.
const SDK_TYPES: &[(&str, &str)] = &[
    (
        "phoxal.robotics.v1.OdometryState",
        "::phoxal::contracts::robotics::OdometryState",
    ),
    (
        "phoxal.robotics.v1.EncoderSample",
        "::phoxal::contracts::component::encoder::EncoderSample",
    ),
    (
        "phoxal.robotics.v1.RangeSample",
        "::phoxal::contracts::component::range::RangeSample",
    ),
    (
        "phoxal.geometry.v1.Vector3",
        "::phoxal::contracts::geometry::Vector3",
    ),
    (
        "phoxal.geometry.v1.Point3",
        "::phoxal::contracts::geometry::Point3",
    ),
    (
        "phoxal.geometry.v1.Quaternion",
        "::phoxal::contracts::geometry::Quaternion",
    ),
    (
        "phoxal.geometry.v1.Pose",
        "::phoxal::contracts::geometry::Pose",
    ),
    (
        "phoxal.geometry.v1.Twist",
        "::phoxal::contracts::geometry::Twist",
    ),
    (
        "phoxal.geometry.v1.Wrench",
        "::phoxal::contracts::geometry::Wrench",
    ),
    (
        "phoxal.component.actuator.v1.Control",
        "::phoxal::contracts::component::actuator::Control",
    ),
    (
        "phoxal.component.actuator.v1.ActuatorTarget",
        "::phoxal::contracts::component::actuator::ActuatorTarget",
    ),
    (
        "phoxal.component.actuator.v1.ActuatorSetpoint",
        "::phoxal::contracts::component::actuator::ActuatorSetpoint",
    ),
    (
        "phoxal.component.imu.v1.AccelerometerSample",
        "::phoxal::contracts::component::imu::AccelerometerSample",
    ),
    (
        "phoxal.component.imu.v1.GyroscopeSample",
        "::phoxal::contracts::component::imu::GyroscopeSample",
    ),
    (
        "phoxal.component.imu.v1.ImuSample",
        "::phoxal::contracts::component::imu::ImuSample",
    ),
    (
        "phoxal.component.camera.v1.ImageEncoding",
        "::phoxal::contracts::component::camera::ImageEncoding",
    ),
    (
        "phoxal.component.camera.v1.CameraFrame",
        "::phoxal::contracts::component::camera::CameraFrame",
    ),
    (
        "phoxal.component.camera.v1.DepthFrame",
        "::phoxal::contracts::component::camera::DepthFrame",
    ),
    (
        "phoxal.component.gnss.v1.GnssSample",
        "::phoxal::contracts::component::gnss::GnssSample",
    ),
    (
        "phoxal.component.battery.v1.BatterySample",
        "::phoxal::contracts::component::battery::BatterySample",
    ),
    (
        "phoxal.component.battery.v1.ChargeState",
        "::phoxal::contracts::component::battery::ChargeState",
    ),
    (
        "phoxal.component.lidar.v1.LaserScan",
        "::phoxal::contracts::component::lidar::LaserScan",
    ),
    (
        "phoxal.bootstrap.v1.SessionOffers",
        "::phoxal::communication::bootstrap::SessionOffers",
    ),
    (
        "phoxal.bootstrap.v1.SessionOffer",
        "::phoxal::communication::bootstrap::SessionOffer",
    ),
    (
        "phoxal.session.v1.OpenSessionRequest",
        "::phoxal::communication::session::OpenSessionRequest",
    ),
    (
        "phoxal.session.v1.OpenSessionResponse",
        "::phoxal::communication::session::OpenSessionResponse",
    ),
    (
        "phoxal.session.v1.RenewSessionRequest",
        "::phoxal::communication::session::RenewSessionRequest",
    ),
    (
        "phoxal.session.v1.RenewSessionResponse",
        "::phoxal::communication::session::RenewSessionResponse",
    ),
    (
        "phoxal.session.v1.CloseSessionRequest",
        "::phoxal::communication::session::CloseSessionRequest",
    ),
    (
        "phoxal.session.v1.CloseSessionResponse",
        "::phoxal::communication::session::CloseSessionResponse",
    ),
    (
        "phoxal.session.v1.SupervisorInfoRequest",
        "::phoxal::communication::session::SupervisorInfoRequest",
    ),
    (
        "phoxal.session.v1.SupervisorInfoResponse",
        "::phoxal::communication::session::SupervisorInfoResponse",
    ),
    (
        "phoxal.session.v1.SupervisorStatusRequest",
        "::phoxal::communication::session::SupervisorStatusRequest",
    ),
    (
        "phoxal.session.v1.SupervisorStatusResponse",
        "::phoxal::communication::session::SupervisorStatusResponse",
    ),
    (
        "phoxal.session.v1.SupervisorState",
        "::phoxal::communication::session::SupervisorState",
    ),
    (
        "phoxal.session.v1.ListExecutionsRequest",
        "::phoxal::communication::session::ListExecutionsRequest",
    ),
    (
        "phoxal.session.v1.ListExecutionsResponse",
        "::phoxal::communication::session::ListExecutionsResponse",
    ),
    (
        "phoxal.session.v1.ExecutionSummary",
        "::phoxal::communication::session::ExecutionSummary",
    ),
    (
        "phoxal.session.v1.ExecutionState",
        "::phoxal::communication::session::ExecutionState",
    ),
    (
        "phoxal.session.v1.ListMethodsRequest",
        "::phoxal::communication::session::ListMethodsRequest",
    ),
    (
        "phoxal.session.v1.ListMethodsResponse",
        "::phoxal::communication::session::ListMethodsResponse",
    ),
    (
        "phoxal.session.v1.MethodMetadata",
        "::phoxal::communication::session::MethodMetadata",
    ),
    (
        "phoxal.session.v1.MethodShape",
        "::phoxal::communication::session::MethodShape",
    ),
    (
        "phoxal.session.v1.BindMethodRequest",
        "::phoxal::communication::session::BindMethodRequest",
    ),
    (
        "phoxal.session.v1.BindMethodResponse",
        "::phoxal::communication::session::BindMethodResponse",
    ),
    (
        "phoxal.session.v1.OperationRequest",
        "::phoxal::communication::session::OperationRequest",
    ),
    (
        "phoxal.session.v1.OperationResponse",
        "::phoxal::communication::session::OperationResponse",
    ),
    (
        "phoxal.session.v1.OperationOutcome",
        "::phoxal::communication::session::OperationOutcome",
    ),
    (
        "phoxal.session.v1.SubscriptionRequest",
        "::phoxal::communication::session::SubscriptionRequest",
    ),
    (
        "phoxal.session.v1.SubscriptionAdmission",
        "::phoxal::communication::session::SubscriptionAdmission",
    ),
    (
        "phoxal.session.v1.SubscriptionRecord",
        "::phoxal::communication::session::SubscriptionRecord",
    ),
    (
        "phoxal.session.v1.RecordKind",
        "::phoxal::communication::session::RecordKind",
    ),
    (
        "phoxal.execution.v1.ContractRequirement",
        "::phoxal::communication::execution::ContractRequirement",
    ),
    (
        "phoxal.execution.v1.ExecutionMode",
        "::phoxal::communication::execution::ExecutionMode",
    ),
    (
        "phoxal.execution.v1.AdmitExecutionRequest",
        "::phoxal::communication::execution::AdmitExecutionRequest",
    ),
    (
        "phoxal.execution.v1.AdmitExecutionResponse",
        "::phoxal::communication::execution::AdmitExecutionResponse",
    ),
    (
        "phoxal.execution.v1.Ready",
        "::phoxal::communication::execution::Ready",
    ),
    (
        "phoxal.execution.v1.InitializeStateRequest",
        "::phoxal::communication::execution::InitializeStateRequest",
    ),
    (
        "phoxal.execution.v1.InitializeStateResponse",
        "::phoxal::communication::execution::InitializeStateResponse",
    ),
    (
        "phoxal.execution.v1.PinReadViewsRequest",
        "::phoxal::communication::execution::PinReadViewsRequest",
    ),
    (
        "phoxal.execution.v1.PinReadViewsResponse",
        "::phoxal::communication::execution::PinReadViewsResponse",
    ),
    (
        "phoxal.execution.v1.Invocation",
        "::phoxal::communication::execution::Invocation",
    ),
    (
        "phoxal.execution.v1.InvocationAccepted",
        "::phoxal::communication::execution::InvocationAccepted",
    ),
    (
        "phoxal.execution.v1.ProductReceipt",
        "::phoxal::communication::execution::ProductReceipt",
    ),
    (
        "phoxal.execution.v1.InputReceipt",
        "::phoxal::communication::execution::InputReceipt",
    ),
    (
        "phoxal.execution.v1.Actuation",
        "::phoxal::communication::execution::Actuation",
    ),
    (
        "phoxal.execution.v1.DeliveryReceipt",
        "::phoxal::communication::execution::DeliveryReceipt",
    ),
    (
        "phoxal.execution.v1.DeliveryAck",
        "::phoxal::communication::execution::DeliveryAck",
    ),
    (
        "phoxal.execution.v1.ResetExecutionRequest",
        "::phoxal::communication::execution::ResetExecutionRequest",
    ),
    (
        "phoxal.execution.v1.ResetExecutionResponse",
        "::phoxal::communication::execution::ResetExecutionResponse",
    ),
    (
        "phoxal.execution.v1.RuntimeFailure",
        "::phoxal::communication::execution::RuntimeFailure",
    ),
    (
        "phoxal.execution.v1.RuntimeWireMetadata",
        "::phoxal::communication::execution::RuntimeWireMetadata",
    ),
    (
        "phoxal.simulation.v1.AcquireAuthorityRequest",
        "::phoxal::communication::simulation::AcquireAuthorityRequest",
    ),
    (
        "phoxal.simulation.v1.ProviderRequirement",
        "::phoxal::communication::simulation::ProviderRequirement",
    ),
    (
        "phoxal.simulation.v1.AcquireAuthorityResponse",
        "::phoxal::communication::simulation::AcquireAuthorityResponse",
    ),
    (
        "phoxal.simulation.v1.TransitionKey",
        "::phoxal::communication::simulation::TransitionKey",
    ),
    (
        "phoxal.simulation.v1.ProductDisposition",
        "::phoxal::communication::simulation::ProductDisposition",
    ),
    (
        "phoxal.simulation.v1.ProductMembership",
        "::phoxal::communication::simulation::ProductMembership",
    ),
    (
        "phoxal.simulation.v1.Observation",
        "::phoxal::communication::simulation::Observation",
    ),
    (
        "phoxal.simulation.v1.Actuation",
        "::phoxal::communication::simulation::Actuation",
    ),
    (
        "phoxal.simulation.v1.ProductReceipt",
        "::phoxal::communication::simulation::ProductReceipt",
    ),
    (
        "phoxal.simulation.v1.PhaseStatus",
        "::phoxal::communication::simulation::PhaseStatus",
    ),
    (
        "phoxal.simulation.v1.CutReceipt",
        "::phoxal::communication::simulation::CutReceipt",
    ),
    (
        "phoxal.simulation.v1.AdmitInitialObservationsRequest",
        "::phoxal::communication::simulation::AdmitInitialObservationsRequest",
    ),
    (
        "phoxal.simulation.v1.AdmitInitialObservationsResponse",
        "::phoxal::communication::simulation::AdmitInitialObservationsResponse",
    ),
    (
        "phoxal.simulation.v1.PrepareBoundaryRequest",
        "::phoxal::communication::simulation::PrepareBoundaryRequest",
    ),
    (
        "phoxal.simulation.v1.PrepareBoundaryResponse",
        "::phoxal::communication::simulation::PrepareBoundaryResponse",
    ),
    (
        "phoxal.simulation.v1.AdmitObservationsRequest",
        "::phoxal::communication::simulation::AdmitObservationsRequest",
    ),
    (
        "phoxal.simulation.v1.AdmitObservationsResponse",
        "::phoxal::communication::simulation::AdmitObservationsResponse",
    ),
    (
        "phoxal.simulation.v1.ResetRequest",
        "::phoxal::communication::simulation::ResetRequest",
    ),
    (
        "phoxal.simulation.v1.ResetResponse",
        "::phoxal::communication::simulation::ResetResponse",
    ),
    (
        "phoxal.simulation.v1.ReleaseAuthorityRequest",
        "::phoxal::communication::simulation::ReleaseAuthorityRequest",
    ),
    (
        "phoxal.simulation.v1.ReleaseAuthorityResponse",
        "::phoxal::communication::simulation::ReleaseAuthorityResponse",
    ),
    (
        "phoxal.simulation.v1.ProgressRequest",
        "::phoxal::communication::simulation::ProgressRequest",
    ),
    (
        "phoxal.simulation.v1.ProgressResponse",
        "::phoxal::communication::simulation::ProgressResponse",
    ),
];

/// The canonical SDK Rust path of one SDK-owned wire identity.
pub(crate) fn sdk_owned_path(fqn: &str) -> Option<&'static str> {
    SDK_TYPES
        .iter()
        .find(|(wire, _)| *wire == fqn)
        .map(|(_, path)| *path)
}

/// The Rust path of a generated definition, relative to its package file
/// when it lives in the same package, absolute through the generated types
/// root otherwise. SDK vocabulary stays in the SDK.
fn referenced_definition(full_name: &str, package: &str, home_package: &str) -> Option<String> {
    if full_name == "google.protobuf.Empty" {
        return None;
    }
    if let Some(path) = sdk_owned_path(full_name) {
        return Some(path.to_owned());
    }
    let rest = full_name.strip_prefix(&format!("{package}."))?;
    let mut parts: Vec<&str> = rest.split('.').collect();
    let last = parts.len().saturating_sub(1);
    let nested: Vec<_> = parts
        .drain(..last)
        .map(ToSnakeCase::to_snake_case)
        .collect();
    let name = rest.rsplit('.').next()?.to_owned();
    if package == home_package {
        let mut segments = nested;
        segments.push(name);
        return Some(segments.join("::"));
    }
    let mut segments: Vec<_> = package.split('.').map(ToSnakeCase::to_snake_case).collect();
    segments.extend(nested);
    segments.push(name);
    Some(format!("crate::api::types::{}", segments.join("::")))
}

/// The Rust path of one generated oneof enum: the package modules, the
/// owning message's nested module chain, and the oneof's CamelCase enum
/// ident.
fn referenced_oneof(
    message_full_name: &str,
    message_name: &str,
    oneof_name: &str,
    package: &str,
    home_package: &str,
) -> Option<String> {
    let rest = message_full_name.strip_prefix(&format!("{package}."))?;
    let mut parts: Vec<&str> = rest.split('.').collect();
    let last = parts.len().saturating_sub(1);
    let nested: Vec<_> = parts
        .drain(..last)
        .map(ToSnakeCase::to_snake_case)
        .collect();
    if package == home_package {
        let mut segments = nested;
        segments.push(message_name.to_snake_case());
        segments.push(oneof_name.to_upper_camel_case());
        return Some(segments.join("::"));
    }
    let mut segments: Vec<_> = package.split('.').map(ToSnakeCase::to_snake_case).collect();
    segments.extend(nested);
    segments.push(message_name.to_snake_case());
    segments.push(oneof_name.to_upper_camel_case());
    Some(format!("crate::api::types::{}", segments.join("::")))
}

/// The Rust path of one generated definition relative to its package
/// file: nested definitions live in the snake-cased parent module.
fn local_path(full_name: &str, package: &str) -> Option<String> {
    let rest = full_name.strip_prefix(&format!("{package}."))?;
    let mut parts: Vec<&str> = rest.split('.').collect();
    let last = parts.len().saturating_sub(1);
    let mut segments: Vec<_> = parts
        .drain(..last)
        .map(ToSnakeCase::to_snake_case)
        .collect();
    segments.push(rest.rsplit('.').next()?.to_owned());
    Some(segments.join("::"))
}

/// The local Rust path of one generated oneof enum: one snake-cased
/// module per nesting level of its owning message, then the oneof's
/// CamelCase enum ident.
fn oneof_local_path(message_full_name: &str, oneof_name: &str, package: &str) -> Option<String> {
    let rest = message_full_name.strip_prefix(&format!("{package}."))?;
    let mut segments: Vec<_> = rest.split('.').map(ToSnakeCase::to_snake_case).collect();
    segments.push(oneof_name.to_upper_camel_case());
    Some(segments.join("::"))
}

/// Emits the schema implementations of every definition in the pool,
/// grouped by generated package file name.
pub fn emit_schema_impls(pool: &DescriptorPool) -> BTreeMap<String, String> {
    let mut by_package: BTreeMap<String, String> = BTreeMap::new();
    for message in pool.all_messages() {
        let package = message.package_name().to_owned();
        if package.is_empty()
            || package.starts_with("google.protobuf")
            || sdk_owned_path(message.full_name()).is_some()
        {
            continue;
        }
        let output = by_package.entry(package.clone()).or_default();
        emit_message(output, &message, &package, true);
        for oneof in message.oneofs() {
            if !oneof.is_synthetic() {
                emit_oneof(output, &message, &oneof, &package, true);
            }
        }
    }
    for enumeration in pool.all_enums() {
        let package = enumeration.package_name().to_owned();
        if package.is_empty()
            || package.starts_with("google.protobuf")
            || sdk_owned_path(enumeration.full_name()).is_some()
        {
            continue;
        }
        let output = by_package.entry(package.clone()).or_default();
        emit_enum(output, &enumeration, &package, true);
    }
    by_package
        .into_iter()
        .filter(|(_, output)| !output.is_empty())
        .map(|(package, output)| {
            (
                package,
                format!("\n// @generated by phoxal-build; do not edit.\n{output}"),
            )
        })
        .collect()
}

fn emit_message(
    output: &mut String,
    message: &MessageDescriptor,
    package: &str,
    retain_frames: bool,
) {
    let Some(path) = local_path(message.full_name(), package) else {
        return;
    };
    let mut referenced: BTreeSet<(String, bool)> = BTreeSet::new();
    let mut fields = String::new();
    for field in message.fields() {
        // Only real oneof members ride the oneof carrier record;
        // synthetic proto3-optional members stay ordinary Optional fields.
        if field
            .containing_oneof()
            .is_some_and(|oneof| !oneof.is_synthetic())
        {
            continue;
        }
        let number = field.number();
        let name = field.name();
        let (ty, reference) = field_type(&field, package);
        if let Some(reference) = reference {
            referenced.insert((reference, false));
        }
        let label = match field.cardinality() {
            Cardinality::Repeated => "Repeated",
            Cardinality::Optional if field.supports_presence() => "Optional",
            _ if matches!(field.kind(), Kind::Message(_)) => "Optional",
            _ => "Singular",
        };
        fields.push_str(&format!(
            "            ::phoxal::schema::FieldRecord {{ number: {number}, name: {name:?}, \
             ty: {ty}, label: ::phoxal::schema::Label::{label}, oneof: None }},\n"
        ));
    }
    for oneof in message.oneofs().filter(|oneof| !oneof.is_synthetic()) {
        let name = oneof.name();
        let Some(number) = oneof.fields().map(|field| field.number()).min() else {
            continue;
        };
        // A payload enum's carrier oneof implements OneofSchema on the
        // public enum itself, which is also this message's Rust type.
        let reference = if name == crate::typed::PAYLOAD_ENUM_ENVELOPE {
            local_path(message.full_name(), package)
        } else {
            referenced_oneof(
                message.full_name(),
                message.name(),
                oneof.name(),
                package,
                package,
            )
        };
        if let Some(reference) = reference {
            referenced.insert((reference, true));
        }
        fields.push_str(&format!(
            "            ::phoxal::schema::FieldRecord {{ number: {number}, name: {name:?}, \
             ty: ::phoxal::schema::FieldType::Oneof, \
             label: ::phoxal::schema::Label::Singular, oneof: Some({name:?}) }},\n"
        ));
    }
    let retention = retention_body(&referenced);
    if retain_frames {
        output.push_str(&frame_static(&format!(
            "<{path} as ::phoxal::schema::MessageSchema>::RECORD"
        )));
    }
    output.push_str(&format!(
        "impl ::phoxal::schema::MessageSchema for {path} {{\n    \
         const RECORD: ::phoxal::schema::SchemaRecord<'static> = \
         ::phoxal::schema::SchemaRecord::Message(::phoxal::schema::MessageRecord {{\n        \
         package: {package:?},\n        name: {name:?},\n        \
         fields: &[\n{fields}        ],\n    }});\n    \
         const WIRE_NAME: &'static str = {wire:?};\n    \
         fn retain_schema() -> usize {{\n{retention}    }}\n}}\n",
        name = message.name(),
        wire = message.full_name(),
    ));
}

fn emit_enum(
    output: &mut String,
    enumeration: &EnumDescriptor,
    package: &str,
    retain_frames: bool,
) {
    let Some(path) = local_path(enumeration.full_name(), package) else {
        return;
    };
    let mut values = String::new();
    for value in enumeration.values() {
        values.push_str(&format!(
            "            ::phoxal::schema::EnumValue {{ number: {}, name: {:?} }},\n",
            value.number(),
            value.name()
        ));
    }
    if retain_frames {
        output.push_str(&frame_static(&format!(
            "<{path} as ::phoxal::schema::MessageSchema>::RECORD"
        )));
    }
    output.push_str(&format!(
        "impl ::phoxal::schema::MessageSchema for {path} {{\n    \
         const RECORD: ::phoxal::schema::SchemaRecord<'static> = \
         ::phoxal::schema::SchemaRecord::Enum(::phoxal::schema::EnumRecord {{\n        \
         package: {package:?},\n        name: {name:?},\n        \
         values: &[\n{values}        ],\n    }});\n    \
         const WIRE_NAME: &'static str = {wire:?};\n    \
         fn retain_schema() -> usize {{ \
         ::phoxal::schema::encoded_len(&Self::RECORD) }}\n}}\n",
        name = enumeration.name(),
        wire = enumeration.full_name(),
    ));
}

fn emit_oneof(
    output: &mut String,
    message: &MessageDescriptor,
    oneof: &prost_reflect::OneofDescriptor,
    package: &str,
    retain_frames: bool,
) {
    // A payload enum's carrier oneof is implemented by the generated
    // public enum, not by a nested oneof module.
    let path = if oneof.name() == crate::typed::PAYLOAD_ENUM_ENVELOPE {
        match local_path(message.full_name(), package) {
            Some(path) => path,
            None => return,
        }
    } else {
        match oneof_local_path(message.full_name(), oneof.name(), package) {
            Some(path) => path,
            None => return,
        }
    };
    let mut referenced: BTreeSet<(String, bool)> = BTreeSet::new();
    let mut variants = String::new();
    let mut tags = Vec::new();
    for field in oneof.fields() {
        let number = field.number();
        let name = field.name();
        let (ty, reference) = field_type(&field, package);
        if let Some(reference) = reference {
            referenced.insert((reference, false));
        }
        variants.push_str(&format!(
            "            ::phoxal::schema::FieldRecord {{ number: {number}, name: {name:?}, \
             ty: {ty}, label: ::phoxal::schema::Label::Singular, oneof: None }},\n"
        ));
        tags.push(number.to_string());
    }
    let retention = retention_body(&referenced);
    if retain_frames {
        output.push_str(&frame_static(&format!(
            "<{path} as ::phoxal::schema::OneofSchema>::RECORD"
        )));
    }
    output.push_str(&format!(
        "impl ::phoxal::schema::OneofSchema for {path} {{\n    \
         const TAGS: &'static [u32] = &[{tags}];\n    \
         const RECORD: ::phoxal::schema::SchemaRecord<'static> = \
         ::phoxal::schema::SchemaRecord::Oneof(::phoxal::schema::OneofRecord {{\n        \
         package: {package:?},\n        message: {message_name:?},\n        \
         field: {field_name:?},\n        \
         variants: &[\n{variants}        ],\n    }});\n    \
         fn retain_schema() -> usize {{\n{retention}    }}\n}}\n",
        tags = tags.join(", "),
        message_name = message.name(),
        field_name = oneof.name(),
    ));
}

/// The schema field type expression of one field, plus the referenced
/// definition's Rust path when the field carries one.
fn field_type(
    field: &prost_reflect::FieldDescriptor,
    home_package: &str,
) -> (String, Option<String>) {
    match field.kind() {
        Kind::Message(message) => {
            let reference =
                referenced_definition(message.full_name(), message.package_name(), home_package);
            (
                format!(
                    "::phoxal::schema::FieldType::Message({:?})",
                    message.full_name()
                ),
                reference,
            )
        }
        Kind::Enum(enumeration) => {
            let reference = referenced_definition(
                enumeration.full_name(),
                enumeration.package_name(),
                home_package,
            );
            (
                format!(
                    "::phoxal::schema::FieldType::Enum({:?})",
                    enumeration.full_name()
                ),
                reference,
            )
        }
        scalar => {
            let kind = match scalar {
                Kind::Double => "Double",
                Kind::Float => "Float",
                Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => "Int32",
                Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => "Int64",
                Kind::Uint32 | Kind::Fixed32 => "Uint32",
                Kind::Uint64 | Kind::Fixed64 => "Uint64",
                Kind::Bool => "Bool",
                Kind::String => "String",
                _ => "Bytes",
            };
            (format!("::phoxal::schema::FieldType::{kind}"), None)
        }
    }
}

/// Emits the linker-retained schema frame of one definition so binaries
/// consuming generated types carry the same descriptor closure as
/// authored messages.
fn frame_static(record: &str) -> String {
    format!(
        "const _: () = {{\n             #[used]\n             #[cfg_attr(target_os = \"macos\", unsafe(link_section = \"__DATA,__phoxal_schema\"))]\n             #[cfg_attr(target_os = \"linux\", unsafe(link_section = \".phoxal_schema\"))]\n             static FRAME: [u8; ::phoxal::schema::encoded_len(&{record})] = {{\n                 let mut bytes = [0_u8; ::phoxal::schema::encoded_len(&{record})];\n                 ::phoxal::schema::write_frame(&{record}, &mut bytes);\n                 bytes\n    }};\n             ::std::hint::black_box(&FRAME);\n}};\n"
    )
}

fn retention_body(referenced: &BTreeSet<(String, bool)>) -> String {
    if referenced.is_empty() {
        return String::from("        ::phoxal::schema::encoded_len(&Self::RECORD)\n");
    }
    let mut body =
        String::from("        let mut size = ::phoxal::schema::encoded_len(&Self::RECORD);\n");
    for (path, oneof) in referenced {
        let schema = if *oneof {
            "OneofSchema"
        } else {
            "MessageSchema"
        };
        body.push_str(&format!(
            "        size += <{path} as ::phoxal::schema::{schema}>::retain_schema();\n"
        ));
    }
    body.push_str("        size\n");
    body
}
