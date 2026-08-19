//! The crate's one contract surface: every endpoint the api tree declares, plus
//! the bus, bundle, participant-metadata and launch records the other owners
//! state beside their own definitions.

use phoxal::__compat::wire::DescribeWire;
use serde_json::Value;

fn surface() -> Value {
    serde_json::from_str(&phoxal::__compat::contract_surface()).expect("the surface is JSON")
}

fn records() -> Vec<Value> {
    surface()["records"]
        .as_array()
        .expect("the surface holds a record array")
        .clone()
}

fn by_path() -> std::collections::BTreeMap<String, Value> {
    records()
        .into_iter()
        .filter(|record| record["record"] == "endpoint")
        .map(|record| {
            (
                record["path"]
                    .as_str()
                    .expect("an endpoint record names its key")
                    .to_owned(),
                record,
            )
        })
        .collect()
}

/// The surface is one JSON document and two calls produce the same bytes,
/// which is what lets a checker compare it with a stored baseline by string
/// equality.
#[test]
fn the_surface_is_deterministic_json() {
    let rendered = phoxal::__compat::contract_surface();
    serde_json::from_str::<Value>(&rendered).expect("the surface is JSON");
    assert_eq!(phoxal::__compat::contract_surface(), rendered);
    assert!(
        !rendered.contains(' '),
        "the rendering carries no whitespace"
    );
}

/// One aggregate, every owner in it. A profile or a module reorganization that
/// dropped an owner would leave the checker comparing a smaller surface against
/// the published one without saying so.
#[test]
fn the_aggregate_holds_every_owner_of_a_process_boundary() {
    let kinds = records()
        .iter()
        .map(|record| {
            record["record"]
                .as_str()
                .expect("a record names its kind")
                .to_owned()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        kinds,
        ["document", "endpoint", "envelope", "identifier", "launch"]
            .into_iter()
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>()
    );

    let names = records()
        .iter()
        .filter_map(|record| record["name"].as_str().map(str::to_owned))
        .collect::<std::collections::BTreeSet<_>>();
    for expected in [
        // The bundle's manifest document and the metadata document every
        // participant binary embeds.
        "ManifestDocument",
        "ParticipantMetadata",
        // The bus envelopes and the frozen key grammar.
        "BusMetadata",
        "QueryFailure",
        "bus-key-composition",
        "encoding",
    ] {
        assert!(names.contains(expected), "{expected} is missing: {names:?}");
    }
}

/// Every record renders once, in canonical order, so a diff against a baseline
/// names the record that moved rather than the whole document.
#[test]
fn records_render_once_each_in_canonical_order() {
    let paths = by_path();
    let recorded = records()
        .iter()
        .filter(|record| record["record"] == "endpoint")
        .count();
    assert_eq!(recorded, paths.len(), "an endpoint appears twice");

    let rendered = phoxal::__compat::contract_surface();
    let mut previous = 0;
    for path in paths.keys() {
        let needle = format!("\"path\":\"{path}\"");
        let at = rendered
            .find(&needle)
            .unwrap_or_else(|| panic!("{path} is missing from the rendering"));
        assert!(at > previous, "{path} is out of canonical order");
        previous = at;
    }
}

/// Specific known records are present, so an accidentally empty or truncated
/// surface cannot pass this suite.
#[test]
fn the_surface_holds_the_records_a_reader_looks_for_first() {
    let by_path = by_path();

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
    let by_path = by_path();

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
