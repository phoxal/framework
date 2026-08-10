//! The generated contract surface: every declared endpoint, with the wire
//! shape of every body it carries.

use phoxal_runtime_contract::wire_schema::DescribeWire;
use serde_json::Value;

fn surface() -> Value {
    serde_json::from_str(&crate::__compat::contract_surface()).expect("the surface is JSON")
}

fn records() -> Vec<Value> {
    surface()["records"]
        .as_array()
        .expect("the surface holds a record array")
        .clone()
}

/// The surface is one JSON document and two calls produce the same bytes,
/// which is what lets a checker compare it with a stored baseline by string
/// equality.
#[test]
fn the_surface_is_deterministic_json() {
    let rendered = crate::__compat::contract_surface();
    serde_json::from_str::<Value>(&rendered).expect("the surface is JSON");
    assert_eq!(crate::__compat::contract_surface(), rendered);
    assert!(
        !rendered.contains(' '),
        "the rendering carries no whitespace"
    );
}

/// The surface and the const catalogue describe the same endpoint set: every
/// declared endpoint appears exactly once, under its own wire key.
#[test]
fn every_declared_endpoint_appears_exactly_once() {
    let declared = crate::API_CONTRACT_MANIFEST
        .iter()
        .flat_map(|family| family.contracts.iter().map(|contract| contract.topic))
        .collect::<std::collections::BTreeSet<_>>();
    let manifest_count = crate::API_CONTRACT_MANIFEST
        .iter()
        .map(|family| family.contracts.len())
        .sum::<usize>();
    assert_eq!(
        declared.len(),
        manifest_count,
        "two endpoints share one wire key, so a surface keyed by it would lose one"
    );

    let recorded = records()
        .iter()
        .map(|record| {
            assert_eq!(record["record"], "endpoint", "{record}");
            record["path"]
                .as_str()
                .expect("an endpoint record names its key")
                .to_string()
        })
        .collect::<Vec<_>>();
    let unique = recorded
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(recorded.len(), unique.len(), "an endpoint appears twice");
    assert_eq!(
        unique,
        declared
            .iter()
            .map(|topic| (*topic).to_string())
            .collect::<std::collections::BTreeSet<_>>()
    );

    let mut sorted = recorded.clone();
    sorted.sort();
    assert_eq!(recorded, sorted, "records are not in canonical order");
}

/// Specific known records are present, so an accidentally empty or truncated
/// surface cannot pass this suite.
#[test]
fn the_surface_holds_the_records_a_reader_looks_for_first() {
    let by_path = records()
        .into_iter()
        .map(|record| {
            (
                record["path"]
                    .as_str()
                    .expect("an endpoint record names its key")
                    .to_string(),
                record,
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    // The frozen bootstrap two binaries exchange before they know whether
    // their trains agree.
    let connect = &by_path["supervisor/connect"];
    assert_eq!(connect["kind"], "query");
    assert_eq!(connect["delivery"], "query");
    assert!(connect["payload"].is_null());
    assert!(connect["request"].is_object() && connect["response"].is_object());

    let target = &by_path["robot/drive/target"];
    assert_eq!(target["family"], "robot");
    assert_eq!(target["kind"], "setpoint");
    assert_eq!(target["delivery"], "setpoint");
    assert!(target["request"].is_null() && target["response"].is_null());

    let state = &by_path["robot/drive/state"];
    assert_eq!(state["kind"], "state");
    assert_eq!(state["delivery"], "state");

    // A dynamic node contributes its variables to the key, not to the body.
    let frame = &by_path["robot/component/{instance}/camera/{capability}/frame"];
    assert_eq!(frame["kind"], "sample");

    // The event kind rides the stream transport, and the surface says both.
    let result = &by_path["robot/navigation/result"];
    assert_eq!(result["kind"], "event");
    assert_eq!(result["delivery"], "stream");
}

/// The schema inside an endpoint record is exactly the payload type's own
/// declared schema: composing the surface must not reshape a body on the way
/// in.
#[test]
fn an_endpoint_record_carries_the_payload_types_own_schema() {
    let by_path = records()
        .into_iter()
        .map(|record| {
            (
                record["path"].as_str().unwrap_or_default().to_string(),
                record,
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    let declared: Value = serde_json::from_str(
        &<crate::robot::drive::Target as DescribeWire>::wire_schema().canonical_json(),
    )
    .expect("a declared schema renders as JSON");
    assert_eq!(by_path["robot/drive/target"]["payload"], declared);

    let request: Value = serde_json::from_str(
        &<crate::supervisor::connect::ConnectRequest as DescribeWire>::wire_schema()
            .canonical_json(),
    )
    .expect("a declared schema renders as JSON");
    assert_eq!(by_path["supervisor/connect"]["request"], request);
}
