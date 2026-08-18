//! The compatibility templates the same declarations emit, and the proof that
//! they are the `0.65` wire surface unchanged.
//!
//! A template is what a concrete key looks like with its dynamic segments left
//! as `{variable}`. Both come out of the same `nodes!`/`endpoints!` structure -
//! [`crate::tree`] pins the concrete side - so a rule that holds for one holds
//! for the other by construction rather than by inspection.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// The dynamic variables the three families declare, spelled exactly as their
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
fn every_endpoint_template_is_unique_across_the_three_families() {
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
            ["robot", "runtime", "supervisor"].contains(&family),
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
        ("runtime/simulation/clock", "event", "stream"),
        ("supervisor/connect", "query", "query"),
        ("supervisor/snapshot", "stream", "stream"),
        ("supervisor/snapshot/current", "query", "query"),
    ] {
        let record = &by_path[path];
        assert_eq!(record["kind"], kind, "{path}");
        assert_eq!(record["delivery"], delivery, "{path}");
    }
}

/// The `0.65` process/wire surface, unchanged.
///
/// The baseline is the five contract surfaces the `0.65` train published, one
/// document per retired library carrier. `0.66` carries them all in one crate,
/// so the proof is that the union of those five documents, put back into the
/// canonical order this crate renders in, is byte-for-byte what this crate now
/// renders.
///
/// Ignored by default and pointed at a directory rather than a checked-in
/// fixture: a published train is the baseline, and this workspace keeps no
/// snapshot fixtures. Run it with
/// `PHOXAL_COMPAT_BASELINE_DIR=<dir> cargo test -p phoxal --test api -- --ignored`.
#[test]
#[ignore = "reads a published-baseline directory named by PHOXAL_COMPAT_BASELINE_DIR"]
fn the_aggregate_is_the_0_65_surface_byte_for_byte() {
    let directory = std::env::var("PHOXAL_COMPAT_BASELINE_DIR")
        .expect("PHOXAL_COMPAT_BASELINE_DIR names the published baseline directory");
    let mut baseline = Vec::new();
    for carrier in [
        "phoxal",
        "phoxal-bundle",
        "phoxal-bus",
        "phoxal-protocol",
        "phoxal-runtime-contract",
    ] {
        let path = std::path::Path::new(&directory).join(format!("{carrier}.json"));
        let document = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} is unreadable: {error}", path.display()));
        baseline.extend(split_records(&document));
    }
    baseline.sort_by_key(|record| sort_key(record));

    let expected = format!("{{\"records\":[{}]}}", baseline.join(","));
    let rendered = phoxal::__compat::contract_surface();
    assert_eq!(
        split_records(&rendered).len(),
        baseline.len(),
        "the aggregate holds a different number of records than the baseline union"
    );
    assert_eq!(rendered, expected);
}

/// Split one canonical surface document into its record substrings, exactly as
/// written.
///
/// The records are compared as the bytes they were published as, so they are
/// never re-rendered on the way through: a records array is scanned with string
/// and bracket depth awareness and cut at the top-level commas.
fn split_records(document: &str) -> Vec<String> {
    const OPEN: &str = "{\"records\":[";
    let body = document
        .strip_prefix(OPEN)
        .and_then(|rest| rest.strip_suffix("]}"))
        .expect("a canonical surface document opens with its records array");
    let mut records = Vec::new();
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut start = 0_usize;
    for (index, character) in body.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            ',' if depth == 0 => {
                records.push(body[start..index].to_owned());
                start = index + 1;
            }
            _ => {}
        }
    }
    if !body.is_empty() {
        records.push(body[start..].to_owned());
    }
    records
}

/// The key one record sorts on, mirroring `ContractRecord::sort_key`: the
/// record's own kind first, then the fields that identify one record within it.
fn sort_key(record: &str) -> (String, String, String) {
    let value: Value = serde_json::from_str(record).expect("a record is JSON");
    let text = |field: &str| value[field].as_str().unwrap_or_default().to_owned();
    match value["record"].as_str().expect("a record names its kind") {
        "endpoint" => ("endpoint".to_owned(), text("family"), text("path")),
        "document" => ("document".to_owned(), text("name"), text("tag")),
        "envelope" => ("envelope".to_owned(), text("name"), String::new()),
        "identifier" => ("identifier".to_owned(), text("name"), String::new()),
        "launch" => ("launch".to_owned(), String::new(), String::new()),
        other => panic!("unknown record kind {other:?}"),
    }
}
