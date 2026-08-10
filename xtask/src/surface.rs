//! What each side of a comparison declares, and what changed between them.
//!
//! A surface document is the canonical JSON one contract-owning crate renders
//! for itself. This module reads a set of them as records, names each record by
//! the fields that make it the *same* record on both sides, and turns the
//! difference into a typed impact plus the field-level detail behind it.
//!
//! Below identity a record is compared as opaque JSON. The checker holds no
//! second copy of the wire-schema model, so a change to that model needs no
//! change here and cannot be half-understood by a stale mirror of it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde_json::Value;

/// One crate that owns a contract surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ContractCrate {
    /// The Cargo and registry package name.
    pub(crate) name: &'static str,
    /// Where this workspace holds it, relative to the workspace root.
    pub(crate) directory: &'static str,
}

impl ContractCrate {
    /// The Rust path the probe project reaches this crate through.
    pub(crate) fn module(self) -> String {
        self.name.replace('-', "_")
    }
}

/// The crates that declare a Phoxal process boundary.
///
/// One train publishes all five together, so they are also the crates whose
/// published versions a baseline is resolved from. A crate that grows a
/// `__compat` module belongs here; one that is absent contributes nothing to a
/// comparison, which is how a boundary would silently stop being checked.
pub(crate) const CONTRACT_CRATES: [ContractCrate; 5] = [
    ContractCrate {
        name: "phoxal",
        directory: "phoxal",
    },
    ContractCrate {
        name: "phoxal-api",
        directory: "crates/api",
    },
    ContractCrate {
        name: "phoxal-bundle",
        directory: "crates/bundle",
    },
    ContractCrate {
        name: "phoxal-bus",
        directory: "crates/bus",
    },
    ContractCrate {
        name: "phoxal-runtime-contract",
        directory: "crates/runtime-contract",
    },
];

/// The kinds of record a surface document carries.
///
/// The set mirrors the one the runtime contract owns. It is restated here
/// rather than imported because the checker depends on no framework crate, and
/// a kind this list does not know is an error rather than a guess: whoever adds
/// a kind states how one record of it is identified, in the same change.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum RecordKind {
    /// One bus endpoint.
    Endpoint,
    /// One persisted or embedded document.
    Document,
    /// One envelope riding beside a body.
    Envelope,
    /// One exact wire constant.
    Identifier,
    /// The argv contract a supervised process is launched with.
    Launch,
}

impl RecordKind {
    /// The kind a record declares itself to be.
    fn of(record: &Value) -> Result<Self> {
        let token = record
            .get("record")
            .and_then(Value::as_str)
            .context("a surface record carries no `record` kind")?;
        match token {
            "endpoint" => Ok(Self::Endpoint),
            "document" => Ok(Self::Document),
            "envelope" => Ok(Self::Envelope),
            "identifier" => Ok(Self::Identifier),
            "launch" => Ok(Self::Launch),
            other => bail!(
                "surface record kind `{other}` is one this checker cannot identify; teach it \
                 which fields name a single record of that kind before declaring one"
            ),
        }
    }

    /// This kind's spelling in a surface document and in a diagnostic.
    fn token(self) -> &'static str {
        match self {
            Self::Endpoint => "endpoint",
            Self::Document => "document",
            Self::Envelope => "envelope",
            Self::Identifier => "identifier",
            Self::Launch => "launch",
        }
    }

    /// The fields that name one record of this kind.
    ///
    /// Identity is what makes two declarations the same record, so a peer
    /// routes or dispatches on exactly these: an endpoint on the family and the
    /// key template, a document on its schema tag, an envelope or a wire
    /// constant on its name. A launch contract needs no key, because a crate
    /// launches its process one way.
    fn identity_key(self, record: &Value) -> Result<String> {
        let field = |name: &str| -> Result<String> {
            record
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .with_context(|| format!("a {} record carries no `{name}`", self.token()))
        };
        match self {
            Self::Endpoint => Ok(format!("{} {}", field("family")?, field("path")?)),
            Self::Document => field("tag"),
            Self::Envelope | Self::Identifier => field("name"),
            Self::Launch => Ok(String::new()),
        }
    }
}

/// What names one record across both sides of a comparison.
///
/// A change to anything outside the identity is a change *to* that record; a
/// change to the identity itself is a removal plus an addition, which is the
/// honest reading, because a peer holding the old key finds nothing.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct RecordIdentity {
    crate_name: String,
    kind: RecordKind,
    key: String,
}

impl RecordIdentity {
    /// Name the record one crate declared.
    fn of(crate_name: &str, record: &Value) -> Result<Self> {
        let kind = RecordKind::of(record)?;
        Ok(Self {
            crate_name: crate_name.to_owned(),
            kind,
            key: kind.identity_key(record)?,
        })
    }
}

impl fmt::Display for RecordIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.crate_name, self.kind.token())?;
        if self.key.is_empty() {
            return Ok(());
        }
        write!(formatter, " {}", self.key)
    }
}

/// Every record one side of a comparison declares, by identity.
#[derive(Clone, Debug, Default)]
pub(crate) struct SurfaceSet {
    records: BTreeMap<RecordIdentity, Value>,
}

impl SurfaceSet {
    /// Read one side's surface documents, keyed by the crate that rendered
    /// each.
    ///
    /// Every contract crate must be present: a missing document is a probe that
    /// silently stopped covering a boundary, not a boundary without records.
    pub(crate) fn read(documents: &BTreeMap<String, Value>) -> Result<Self> {
        let mut records = BTreeMap::new();
        for contract_crate in CONTRACT_CRATES {
            let document = documents
                .get(contract_crate.name)
                .with_context(|| format!("{} rendered no contract surface", contract_crate.name))?;
            let declared = document
                .get("records")
                .and_then(Value::as_array)
                .with_context(|| {
                    format!(
                        "{}'s contract surface holds no records",
                        contract_crate.name
                    )
                })?;
            for record in declared {
                let identity = RecordIdentity::of(contract_crate.name, record)?;
                if records.insert(identity.clone(), record.clone()).is_some() {
                    bail!("{identity} is declared twice in one contract surface");
                }
            }
        }
        Ok(Self { records })
    }

    /// Every record-level difference from this baseline to `current`, in a
    /// stable order.
    pub(crate) fn changes_to(&self, current: &Self) -> Vec<RecordChange> {
        let mut changes = Vec::new();
        for (identity, baseline) in &self.records {
            match current.records.get(identity) {
                None => changes.push(RecordChange {
                    identity: identity.clone(),
                    detail: ChangeDetail::Removed,
                }),
                Some(declared) if declared != baseline => changes.push(RecordChange {
                    identity: identity.clone(),
                    detail: ChangeDetail::Changed {
                        differences: FieldDifference::between(baseline, declared),
                    },
                }),
                Some(_) => {}
            }
        }
        changes.extend(
            current
                .records
                .keys()
                .filter(|identity| !self.records.contains_key(*identity))
                .map(|identity| RecordChange {
                    identity: identity.clone(),
                    detail: ChangeDetail::Added,
                }),
        );
        changes.sort_by(|left, right| left.identity.cmp(&right.identity));
        changes
    }
}

/// One record that is not declared the same way on both sides.
#[derive(Clone, Debug)]
pub(crate) struct RecordChange {
    identity: RecordIdentity,
    detail: ChangeDetail,
}

impl RecordChange {
    /// Whether this change can break a peer built against the baseline.
    ///
    /// A record that is gone or declared differently breaks one; a record that
    /// did not exist yet cannot, because nothing was built against it.
    fn breaks_a_peer(&self) -> bool {
        matches!(
            self.detail,
            ChangeDetail::Removed | ChangeDetail::Changed { .. }
        )
    }
}

impl fmt::Display for RecordChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:<8} {}", self.detail.token(), self.identity)?;
        let ChangeDetail::Changed { differences } = &self.detail else {
            return Ok(());
        };
        for difference in differences {
            write!(formatter, "\n             {difference}")?;
        }
        Ok(())
    }
}

/// How one record differs between the two sides.
#[derive(Clone, Debug)]
enum ChangeDetail {
    /// The baseline declares the record and the workspace no longer does.
    Removed,
    /// The workspace declares a record the baseline never had.
    Added,
    /// Both declare it, differently.
    Changed {
        /// The first places the two declarations disagree.
        differences: Vec<FieldDifference>,
    },
}

impl ChangeDetail {
    fn token(&self) -> &'static str {
        match self {
            Self::Removed => "removed",
            Self::Added => "added",
            Self::Changed { .. } => "changed",
        }
    }
}

/// How many differences one changed record reports.
///
/// A replaced body disagrees at every leaf beneath it; naming the change takes
/// the first few, and printing the rest buries it.
const REPORTED_DIFFERENCES: usize = 5;

/// How long a value is allowed to be in a diagnostic before it is cut.
const REPORTED_VALUE_LENGTH: usize = 72;

/// One place inside a changed record where the two declarations disagree.
#[derive(Clone, Debug)]
pub(crate) struct FieldDifference {
    path: String,
    baseline: String,
    current: String,
}

impl FieldDifference {
    /// The first places two declarations of one record disagree, named by their
    /// JSON path into the record.
    fn between(baseline: &Value, current: &Value) -> Vec<Self> {
        let mut found = Vec::new();
        Self::walk("$", Some(baseline), Some(current), &mut found);
        found
    }

    fn walk(path: &str, baseline: Option<&Value>, current: Option<&Value>, found: &mut Vec<Self>) {
        if found.len() >= REPORTED_DIFFERENCES || baseline == current {
            return;
        }
        match (baseline, current) {
            (Some(Value::Object(left)), Some(Value::Object(right))) => {
                for key in left.keys().chain(right.keys()).collect::<BTreeSet<_>>() {
                    Self::walk(
                        &format!("{path}.{key}"),
                        left.get(key),
                        right.get(key),
                        found,
                    );
                }
            }
            (Some(Value::Array(left)), Some(Value::Array(right))) => {
                for index in 0..left.len().max(right.len()) {
                    Self::walk(
                        &format!("{path}[{index}]"),
                        left.get(index),
                        right.get(index),
                        found,
                    );
                }
            }
            _ => found.push(Self {
                path: path.to_owned(),
                baseline: Self::render(baseline),
                current: Self::render(current),
            }),
        }
    }

    /// One side of a difference, short enough to read on one line.
    fn render(value: Option<&Value>) -> String {
        let Some(value) = value else {
            return "absent".to_owned();
        };
        let rendered = value.to_string();
        if rendered.chars().count() <= REPORTED_VALUE_LENGTH {
            return rendered;
        }
        let cut = rendered
            .char_indices()
            .nth(REPORTED_VALUE_LENGTH)
            .map_or(rendered.len(), |(index, _)| index);
        format!("{}...", &rendered[..cut])
    }
}

impl fmt::Display for FieldDifference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {} -> {}",
            self.path, self.baseline, self.current
        )
    }
}

/// What a set of changes does to a peer built against the baseline.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, ValueEnum)]
pub(crate) enum CompatibilityImpact {
    /// Every baseline record is still declared, identically, and nothing was
    /// added.
    #[default]
    Unchanged,
    /// Records were added and every record the baseline already had is
    /// declared identically.
    Additive,
    /// A record the baseline declared is gone or declared differently.
    Breaking,
}

impl CompatibilityImpact {
    /// The impact a change set carries.
    pub(crate) fn of(changes: &[RecordChange]) -> Self {
        if changes.iter().any(RecordChange::breaks_a_peer) {
            Self::Breaking
        } else if changes.is_empty() {
            Self::Unchanged
        } else {
            Self::Additive
        }
    }

    /// This impact's spelling in a report and on the command line.
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Additive => "additive",
            Self::Breaking => "breaking",
        }
    }
}

impl fmt::Display for CompatibilityImpact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.token())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// One endpoint record, carrying a payload with a single named field.
    fn endpoint(path: &str, field: &str, field_kind: &str) -> Value {
        json!({
            "delivery": "state",
            "family": "robot",
            "kind": "state",
            "path": path,
            "payload": {
                "fields": [{"name": field, "schema": {"kind": field_kind}}],
                "kind": "struct",
            },
            "record": "endpoint",
            "request": Value::Null,
            "response": Value::Null,
        })
    }

    fn identifier(name: &str, value: &str) -> Value {
        json!({"name": name, "record": "identifier", "value": value})
    }

    fn launch(argument: &str, required: bool) -> Value {
        json!({
            "arguments": [{
                "name": argument,
                "repeated": false,
                "required": required,
                "value": "text",
            }],
            "record": "launch",
        })
    }

    fn document() -> Value {
        json!({
            "body": {"fields": [], "kind": "struct"},
            "name": "RuntimeDocument",
            "record": "document",
            "tag": "phoxal/runtime-bundle/v0",
        })
    }

    fn envelope() -> Value {
        json!({"body": {"fields": [], "kind": "struct"}, "name": "BusMetadata", "record": "envelope"})
    }

    /// A full side of a comparison: every contract crate present, each with the
    /// records that crate owns.
    fn side(
        endpoints: Vec<Value>,
        constants: Vec<Value>,
        launch: Value,
    ) -> BTreeMap<String, Value> {
        let bus = std::iter::once(envelope())
            .chain(constants)
            .collect::<Vec<_>>();
        BTreeMap::from([
            ("phoxal".to_owned(), json!({"records": [launch]})),
            ("phoxal-api".to_owned(), json!({"records": endpoints})),
            ("phoxal-bundle".to_owned(), json!({"records": [document()]})),
            ("phoxal-bus".to_owned(), json!({"records": bus})),
            (
                "phoxal-runtime-contract".to_owned(),
                json!({"records": [document()]}),
            ),
        ])
    }

    fn baseline() -> BTreeMap<String, Value> {
        side(
            vec![endpoint("robot/drive/state", "speed", "f64")],
            vec![identifier("bus-encoding", "phoxal/v0;codec=1")],
            launch("execution-id", true),
        )
    }

    fn compare(current: &BTreeMap<String, Value>) -> (CompatibilityImpact, Vec<RecordChange>) {
        let changes = SurfaceSet::read(&baseline())
            .expect("the baseline reads")
            .changes_to(&SurfaceSet::read(current).expect("the current side reads"));
        (CompatibilityImpact::of(&changes), changes)
    }

    fn rendered(changes: &[RecordChange]) -> String {
        changes
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Two identical sides are unchanged, which is what lets an ordinary
    /// release ride a patch.
    #[test]
    fn identical_surfaces_are_unchanged() {
        let (impact, changes) = compare(&baseline());
        assert_eq!(impact, CompatibilityImpact::Unchanged);
        assert!(changes.is_empty(), "{}", rendered(&changes));
    }

    /// A new endpoint cannot break a peer that never knew it existed.
    #[test]
    fn a_new_endpoint_is_additive() {
        let (impact, changes) = compare(&side(
            vec![
                endpoint("robot/drive/state", "speed", "f64"),
                endpoint("robot/drive/command", "speed", "f64"),
            ],
            vec![identifier("bus-encoding", "phoxal/v0;codec=1")],
            launch("execution-id", true),
        ));
        assert_eq!(impact, CompatibilityImpact::Additive);
        let report = rendered(&changes);
        assert!(
            report.contains("added    phoxal-api endpoint robot robot/drive/command"),
            "{report}"
        );
    }

    /// A field that changed type is a break, and the diagnostic names the
    /// record and the path inside it rather than only that something moved.
    #[test]
    fn a_changed_payload_field_is_breaking_and_names_its_path() {
        let (impact, changes) = compare(&side(
            vec![endpoint("robot/drive/state", "speed", "u32")],
            vec![identifier("bus-encoding", "phoxal/v0;codec=1")],
            launch("execution-id", true),
        ));
        assert_eq!(impact, CompatibilityImpact::Breaking);
        let report = rendered(&changes);
        assert!(
            report.contains("changed  phoxal-api endpoint robot robot/drive/state"),
            "{report}"
        );
        assert!(
            report.contains(r#"$.payload.fields[0].schema.kind: "f64" -> "u32""#),
            "{report}"
        );
    }

    /// A renamed field is a removal plus an addition inside one record, which
    /// the path-level diagnostic names on both sides.
    #[test]
    fn a_renamed_payload_field_is_breaking() {
        let (impact, changes) = compare(&side(
            vec![endpoint("robot/drive/state", "velocity", "f64")],
            vec![identifier("bus-encoding", "phoxal/v0;codec=1")],
            launch("execution-id", true),
        ));
        assert_eq!(impact, CompatibilityImpact::Breaking);
        let report = rendered(&changes);
        assert!(
            report.contains(r#"$.payload.fields[0].name: "speed" -> "velocity""#),
            "{report}"
        );
    }

    /// A peer that still publishes on a removed key talks to nobody.
    #[test]
    fn a_removed_endpoint_is_breaking() {
        let (impact, changes) = compare(&side(
            Vec::new(),
            vec![identifier("bus-encoding", "phoxal/v0;codec=1")],
            launch("execution-id", true),
        ));
        assert_eq!(impact, CompatibilityImpact::Breaking);
        let report = rendered(&changes);
        assert!(
            report.contains("removed  phoxal-api endpoint robot robot/drive/state"),
            "{report}"
        );
    }

    /// A wire constant is spelled identically by both peers or by neither.
    #[test]
    fn a_changed_identifier_value_is_breaking() {
        let (impact, changes) = compare(&side(
            vec![endpoint("robot/drive/state", "speed", "f64")],
            vec![identifier("bus-encoding", "phoxal/v1;codec=1")],
            launch("execution-id", true),
        ));
        assert_eq!(impact, CompatibilityImpact::Breaking);
        let report = rendered(&changes);
        assert!(
            report.contains("changed  phoxal-bus identifier bus-encoding"),
            "{report}"
        );
        assert!(
            report.contains(r#"$.value: "phoxal/v0;codec=1" -> "phoxal/v1;codec=1""#),
            "{report}"
        );
    }

    /// A supervisor launching the previous argv is as broken as a peer holding
    /// the previous key.
    #[test]
    fn a_changed_launch_argument_is_breaking() {
        let (impact, changes) = compare(&side(
            vec![endpoint("robot/drive/state", "speed", "f64")],
            vec![identifier("bus-encoding", "phoxal/v0;codec=1")],
            launch("execution", true),
        ));
        assert_eq!(impact, CompatibilityImpact::Breaking);
        let report = rendered(&changes);
        assert!(report.contains("changed  phoxal launch"), "{report}");
        assert!(
            report.contains(r#"$.arguments[0].name: "execution-id" -> "execution""#),
            "{report}"
        );
    }

    /// An argument that stopped being required changes the same record without
    /// touching its identity.
    #[test]
    fn a_relaxed_launch_argument_is_breaking() {
        let (impact, changes) = compare(&side(
            vec![endpoint("robot/drive/state", "speed", "f64")],
            vec![identifier("bus-encoding", "phoxal/v0;codec=1")],
            launch("execution-id", false),
        ));
        assert_eq!(impact, CompatibilityImpact::Breaking);
        assert!(
            rendered(&changes).contains("$.arguments[0].required: true -> false"),
            "{}",
            rendered(&changes)
        );
    }

    /// A record kind the checker cannot identify is refused rather than
    /// compared by a guess, because a guessed identity reports a break that did
    /// not happen or hides one that did.
    #[test]
    fn an_unknown_record_kind_is_refused() {
        let mut documents = baseline();
        documents.insert(
            "phoxal-bus".to_owned(),
            json!({"records": [{"record": "capability", "name": "drive"}]}),
        );
        let failure = SurfaceSet::read(&documents)
            .expect_err("an unidentifiable record kind cannot be compared")
            .to_string();
        assert!(failure.contains("`capability`"), "{failure}");
    }

    /// A missing crate is a probe that stopped covering a boundary, so it is an
    /// error rather than a boundary with no records.
    #[test]
    fn a_missing_contract_crate_is_refused() {
        let mut documents = baseline();
        documents.remove("phoxal-bus");
        let failure = SurfaceSet::read(&documents)
            .expect_err("an absent crate cannot be compared")
            .to_string();
        assert!(failure.contains("phoxal-bus"), "{failure}");
    }

    /// A record whose whole body was replaced reports enough to name the change
    /// and stops, rather than printing the tree beneath it.
    #[test]
    fn a_wholly_replaced_body_reports_a_bounded_number_of_differences() {
        let mut wide = endpoint("robot/drive/state", "speed", "f64");
        wide["payload"]["fields"] = json!(
            (0..20)
                .map(|index| json!({"name": format!("field{index}"), "schema": {"kind": "u8"}}))
                .collect::<Vec<_>>()
        );
        let (impact, changes) = compare(&side(
            vec![wide],
            vec![identifier("bus-encoding", "phoxal/v0;codec=1")],
            launch("execution-id", true),
        ));
        assert_eq!(impact, CompatibilityImpact::Breaking);
        assert_eq!(
            rendered(&changes).lines().count(),
            1 + REPORTED_DIFFERENCES,
            "{}",
            rendered(&changes)
        );
    }

    /// The impact order is what makes a declared impact able to raise a
    /// mechanical one and unable to lower it.
    #[test]
    fn impacts_order_from_unchanged_to_breaking() {
        assert!(CompatibilityImpact::Unchanged < CompatibilityImpact::Additive);
        assert!(CompatibilityImpact::Additive < CompatibilityImpact::Breaking);
    }
}
