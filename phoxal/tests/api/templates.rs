//! The compatibility templates the current declarations emit.
//!
//! A template is what a concrete key looks like with its dynamic segments left
//! as `{variable}`. Both come out of the same `nodes!`/`endpoints!` structure -
//! [`crate::tree`] pins the concrete side - so a rule that holds for one holds
//! for the other by construction rather than by inspection.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// The dynamic variables the four families declare, spelled exactly as their
/// `nodes!` declarations bind them.
const DECLARED_VARIABLES: [&str; 3] = ["capability", "instance", "joint"];

fn records() -> Vec<Value> {
    let surface: Value =
        serde_json::from_str(&phoxal::__compat::contract_surface()).expect("the surface is JSON");
    surface["records"]
        .as_array()
        .expect("the surface holds a record array")
        .clone()
}

fn endpoint_records() -> Vec<Value> {
    records()
        .into_iter()
        .filter(|record| record["record"] == "endpoint")
        .collect()
}

fn template(record: &Value) -> &str {
    record["path"]
        .as_str()
        .expect("an endpoint record names its key template")
}

/// One endpoint, one key. Two endpoints sharing a template would be two
/// contracts a receiver's per-key subscription could not tell apart.
#[test]
fn every_endpoint_template_is_unique_across_the_four_families() {
    let declared = endpoint_records();
    let unique = declared.iter().map(template).collect::<BTreeSet<_>>();
    assert_eq!(
        declared.len(),
        unique.len(),
        "two endpoints render the same key template"
    );
}

/// A family is the leading segment of every key it declares, so a key can never
/// land in another family's Zenoh subtree.
#[test]
fn every_template_is_rooted_at_its_own_family() {
    for record in endpoint_records() {
        let family = record["family"]
            .as_str()
            .expect("an endpoint record names its family");
        assert!(
            ["robot", "runtime", "simulation", "supervisor"].contains(&family),
            "{family} is not a declared family"
        );
        let path = template(&record);
        assert!(
            path == family || path.starts_with(&format!("{family}/")),
            "{path} is not rooted at the {family} family"
        );
    }
}

/// Every placeholder is one declared dynamic variable, and it appears once: a
/// template that bound the same variable twice would render two segments from
/// one value.
#[test]
fn every_placeholder_is_a_declared_variable_bound_exactly_once() {
    for record in endpoint_records() {
        let path = template(&record);
        let mut seen = Vec::new();
        for segment in path.split('/') {
            let Some(variable) = segment
                .strip_prefix('{')
                .and_then(|rest| rest.strip_suffix('}'))
            else {
                assert!(
                    !segment.contains('{') && !segment.contains('}'),
                    "{path} holds a malformed placeholder segment {segment:?}"
                );
                continue;
            };
            assert!(
                DECLARED_VARIABLES.contains(&variable),
                "{path} binds {variable:?}, which no node declares"
            );
            assert!(
                !seen.contains(&variable),
                "{path} binds {variable:?} more than once"
            );
            seen.push(variable);
        }
    }
}

/// The kind and the delivery lane on a record are the ones the endpoint's
/// semantic fixes, including the world clock, whose distinct authority keeps
/// the ordinary event wire kind.
#[test]
fn a_records_kind_and_lane_are_the_ones_its_semantic_fixes() {
    let by_path = endpoint_records()
        .into_iter()
        .map(|record| (template(&record).to_owned(), record))
        .collect::<BTreeMap<_, _>>();

    for (path, kind, delivery) in [
        ("robot/drive/state", "state", "state"),
        ("robot/drive/target", "setpoint", "setpoint"),
        ("robot/navigation/result", "event", "stream"),
        (
            "robot/component/{instance}/camera/{capability}/frame",
            "sample",
            "sample",
        ),
        (
            "robot/component/{instance}/speaker/{capability}/stream",
            "stream",
            "stream",
        ),
        ("runtime/logs", "stream", "stream"),
        // The world clock: a distinct Rust authority, the same wire kind it has
        // always had.
        ("simulation/clock", "event", "stream"),
        ("supervisor/connect", "query", "query"),
        ("supervisor/snapshot", "stream", "stream"),
        ("supervisor/snapshot/current", "query", "query"),
    ] {
        let record = &by_path[path];
        assert_eq!(record["kind"], kind, "{path}");
        assert_eq!(record["delivery"], delivery, "{path}");
    }
}
