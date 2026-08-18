//! What each side of a comparison declares, and what changed between them.
//!
//! A surface document is the canonical JSON a contract-owning crate renders for
//! itself. This module reads the documents one side stated as records, names
//! each record by the fields that make it the *same* record on both sides, and
//! turns the difference into a typed impact plus the field-level detail behind
//! it.
//!
//! A side normally states one document, because the framework is one library.
//! It states several only for a baseline that predates the merge (see
//! [`crate::legacy`]), and the records are unioned either way: identity carries
//! no crate name, so a record that changed which crate declares it is still the
//! same record. That is the honest reading - a peer routes on the record, and
//! has never been able to see which Cargo package the other side compiled it
//! from.
//!
//! Below identity a record is compared as opaque JSON. The checker holds no
//! second copy of the wire-schema model, so a change to that model needs no
//! change here and cannot be half-understood by a stale mirror of it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde_json::Value;

/// The one crate that declares the Phoxal process/wire boundary.
///
/// It is the whole framework library, so it is also the crate whose published
/// versions a baseline is resolved from: one train, one carrier, one release
/// decision.
pub(crate) const CONTRACT_CRATE: &str = "phoxal";

/// Where this workspace holds [`CONTRACT_CRATE`], relative to the workspace
/// root.
pub(crate) const CONTRACT_CRATE_DIRECTORY: &str = "phoxal";

/// The records the attachment bootstrap freezes, by the identity a comparison
/// names them under.
///
/// `supervisor/connect` is the one exchange two Phoxal binaries complete before
/// they know whether they agree on anything else, so its record covers the key
/// spelling, both frozen document shapes, and the canonical framework-version
/// spelling inside them.
///
/// The rest of the list is the transport that exchange stands on: everything an
/// attaching client traverses *before* it can decode that reply. A future line
/// that moved the key grammar, the query envelopes, the encoding or the Zenoh
/// wire protocol would leave the frozen documents intact and unreachable, and
/// the promise would be broken with no gate noticing. Nothing here may move in
/// any release of any line, and a change that moves one is the single failure
/// with no autonomous remedy - so the checker names it as such instead of
/// leaving the reader to notice which of many records it was.
///
/// `identifier participant-ready-key` is deliberately absent: a client composes
/// it only after the bootstrap has already answered.
///
/// The set is restated here for the same reason the record kinds are: the
/// checker depends on no framework crate. It cannot go stale silently - a
/// renamed or removed frozen record loses this identity, which the comparison
/// reports as a removal and this list still recognizes.
const FROZEN_RECORDS: [(RecordKind, &str); 7] = [
    (RecordKind::Endpoint, "supervisor supervisor/connect"),
    (RecordKind::Envelope, "BusMetadata"),
    (RecordKind::Envelope, "QueryFailure"),
    (RecordKind::Identifier, "bus-key-composition"),
    (RecordKind::Identifier, "bus-key-root"),
    (RecordKind::Identifier, "encoding"),
    (RecordKind::Identifier, "zenoh-wire-protocol"),
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
///
/// The crate that declared the record is deliberately not part of it. Nothing
/// on the wire carries a Cargo package name, so a record that moves between
/// modules - or between the libraries a train used to publish - is the same
/// record, and the comparison says so.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct RecordIdentity {
    kind: RecordKind,
    key: String,
}

impl RecordIdentity {
    /// Name one declared record.
    fn of(record: &Value) -> Result<Self> {
        let kind = RecordKind::of(record)?;
        Ok(Self {
            kind,
            key: kind.identity_key(record)?,
        })
    }
}

impl fmt::Display for RecordIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.kind.token())?;
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
    /// Read one side's surface documents as one record set.
    ///
    /// A side states one document per crate it was read from - one normally,
    /// five for a baseline that predates the merge - and the union is what that
    /// side declares. Two documents naming one record is a probe that read the
    /// same boundary twice, which would hide a difference in one of the copies,
    /// so it is refused rather than resolved.
    pub(crate) fn read(documents: &[Value]) -> Result<Self> {
        if documents.is_empty() {
            bail!("a side of the comparison stated no contract surface at all");
        }
        let mut records = BTreeMap::new();
        for document in documents {
            let declared = document
                .get("records")
                .and_then(Value::as_array)
                .context("a contract surface holds no records")?;
            for record in declared {
                let identity = RecordIdentity::of(record)?;
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

    /// Whether this change moves a record the attachment bootstrap freezes.
    ///
    /// Only a break counts. A record added beside a frozen one leaves the
    /// frozen one exactly where it was, which is what the freeze protects.
    pub(crate) fn breaks_the_frozen_bootstrap(&self) -> bool {
        self.breaks_a_peer()
            && FROZEN_RECORDS
                .iter()
                .any(|(kind, key)| self.identity.kind == *kind && self.identity.key == *key)
    }
}

impl fmt::Display for RecordChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:<8} {}", self.detail.token(), self.identity)?;
        if self.breaks_the_frozen_bootstrap() {
            write!(formatter, "  [FROZEN BOOTSTRAP]")?;
        }
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FieldDifference {
    path: String,
    baseline: String,
    current: String,
}

impl FieldDifference {
    /// The first places two declarations of one record disagree, named by their
    /// JSON path into the record.
    ///
    /// The authored-source leg reads it for the same job on a different
    /// document: two compilations of one authored project that disagree about
    /// what it means.
    pub(crate) fn between(baseline: &Value, current: &Value) -> Vec<Self> {
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

    fn document(name: &str, tag: &str) -> Value {
        json!({
            "body": {"fields": [], "kind": "struct"},
            "name": name,
            "record": "document",
            "tag": tag,
        })
    }

    fn envelope() -> Value {
        json!({"body": {"fields": [], "kind": "struct"}, "name": "BusMetadata", "record": "envelope"})
    }

    /// One side of a comparison: the one document the framework library states,
    /// holding every record the side declares.
    fn side(endpoints: Vec<Value>, constants: Vec<Value>, launch: Value) -> Vec<Value> {
        let records = std::iter::once(launch)
            .chain(std::iter::once(envelope()))
            .chain(std::iter::once(document(
                "ManifestDocument",
                "phoxal/manifest/v0",
            )))
            .chain(endpoints)
            .chain(constants)
            .collect::<Vec<_>>();
        vec![json!({ "records": records })]
    }

    fn baseline() -> Vec<Value> {
        side(
            vec![endpoint("robot/drive/state", "speed", "f64")],
            vec![identifier("bus-encoding", "phoxal/v0;codec=1")],
            launch("execution-id", true),
        )
    }

    fn compare(current: &[Value]) -> (CompatibilityImpact, Vec<RecordChange>) {
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
            report.contains("added    endpoint robot robot/drive/command"),
            "{report}"
        );
    }

    /// The diagnostic names the exact record and path rather than only that
    /// something moved.
    #[test]
    fn an_exact_endpoint_change_stays_visible() {
        let (impact, changes) = compare(&side(
            vec![endpoint("robot/drive/state", "speed", "u32")],
            vec![identifier("bus-encoding", "phoxal/v0;codec=1")],
            launch("execution-id", true),
        ));
        assert_eq!(impact, CompatibilityImpact::Breaking);
        let report = rendered(&changes);
        assert!(
            report.contains("changed  endpoint robot robot/drive/state"),
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

    /// A removed endpoint is a break: a peer that still publishes on that key
    /// talks to nobody.
    #[test]
    fn an_endpoint_removal_stays_visible() {
        let (impact, changes) = compare(&side(
            Vec::new(),
            vec![identifier("bus-encoding", "phoxal/v0;codec=1")],
            launch("execution-id", true),
        ));
        assert_eq!(impact, CompatibilityImpact::Breaking);
        let report = rendered(&changes);
        assert!(
            report.contains("removed  endpoint robot robot/drive/state"),
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
            report.contains("changed  identifier bus-encoding"),
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
        assert!(report.contains("changed  launch"), "{report}");
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

    /// The merge itself: the last multi-crate train stated the same records
    /// from five documents that this workspace states from one. Identity
    /// carries no crate name, so the union is the same record set and the
    /// comparison sees no change at all - which is exactly the evidence the
    /// merge release has to produce.
    #[test]
    fn a_legacy_baseline_of_five_documents_unions_to_the_single_carrier_surface() {
        let legacy = vec![
            json!({"records": [launch("execution-id", true)]}),
            json!({"records": [endpoint("robot/drive/state", "speed", "f64")]}),
            json!({"records": [
                envelope(),
                identifier("bus-encoding", "phoxal/v0;codec=1"),
            ]}),
            json!({"records": [document("ManifestDocument", "phoxal/manifest/v0")]}),
            json!({"records": [document("ParticipantMetadata", "phoxal/participant-metadata/v0")]}),
        ];
        let mut merged = baseline();
        let records = merged[0]["records"]
            .as_array_mut()
            .expect("the merged side holds records");
        records.push(document(
            "ParticipantMetadata",
            "phoxal/participant-metadata/v0",
        ));

        let changes = SurfaceSet::read(&legacy)
            .expect("the legacy union reads")
            .changes_to(&SurfaceSet::read(&merged).expect("the merged surface reads"));
        assert!(changes.is_empty(), "{}", rendered(&changes));
        assert_eq!(
            CompatibilityImpact::of(&changes),
            CompatibilityImpact::Unchanged
        );
    }

    /// Two documents naming one record is a probe that read a boundary twice,
    /// which would hide a difference in the copy it discarded.
    #[test]
    fn a_record_declared_by_two_documents_is_refused() {
        let duplicated = vec![
            json!({"records": [identifier("encoding", "phoxal/v0;codec=1")]}),
            json!({"records": [identifier("encoding", "phoxal/v0;codec=1")]}),
        ];
        let failure = SurfaceSet::read(&duplicated)
            .expect_err("one record cannot be declared twice")
            .to_string();
        assert!(failure.contains("identifier encoding"), "{failure}");
    }

    /// A record kind the checker cannot identify is refused rather than
    /// compared by a guess, because a guessed identity reports a break that did
    /// not happen or hides one that did.
    #[test]
    fn an_unknown_record_kind_is_refused() {
        let failure = SurfaceSet::read(&[json!({
            "records": [{"record": "capability", "name": "drive"}]
        })])
        .expect_err("an unidentifiable record kind cannot be compared")
        .to_string();
        assert!(failure.contains("`capability`"), "{failure}");
    }

    /// A side that stated nothing at all is a probe that stopped covering the
    /// boundary, not a boundary without records.
    #[test]
    fn a_side_that_stated_no_document_is_refused() {
        let failure = SurfaceSet::read(&[])
            .expect_err("an empty side cannot be compared")
            .to_string();
        assert!(failure.contains("no contract surface"), "{failure}");
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

    /// The execution supervisor consumes contracts but never supplies a
    /// contract carrier or a second compatibility identity.
    #[test]
    fn the_supervisor_is_not_a_contract_carrier() {
        assert_eq!(CONTRACT_CRATE, "phoxal");
        assert_ne!(CONTRACT_CRATE, "phoxal-supervisor");
        assert_ne!(CONTRACT_CRATE_DIRECTORY, "supervisor");
    }

    /// The bootstrap endpoint, spelled the way the contract crate renders it.
    fn connect() -> Value {
        json!({
            "delivery": "query",
            "family": "supervisor",
            "kind": "query",
            "path": "supervisor/connect",
            "payload": Value::Null,
            "record": "endpoint",
            "request": {"fields": [], "kind": "struct"},
            "response": {"fields": [], "kind": "struct"},
        })
    }

    fn changes_between(published: Vec<Value>, current: Vec<Value>) -> Vec<RecordChange> {
        let side = |endpoints| {
            side(
                endpoints,
                vec![identifier("bus-encoding", "phoxal/v0;codec=1")],
                launch("execution-id", true),
            )
        };
        SurfaceSet::read(&side(published))
            .expect("the baseline reads")
            .changes_to(&SurfaceSet::read(&side(current)).expect("the current side reads"))
    }

    /// A break at the frozen bootstrap is called out by name: it is the one
    /// finding with no autonomous remedy, so a reader must not have to work out
    /// which record moved.
    #[test]
    fn a_break_at_the_frozen_bootstrap_is_named_as_one() {
        let mut moved = connect();
        moved["response"] = json!({"fields": [{"name": "verdict"}], "kind": "struct"});
        for current in [vec![], vec![moved]] {
            let changes = changes_between(vec![connect()], current);
            assert!(
                changes
                    .iter()
                    .any(RecordChange::breaks_the_frozen_bootstrap),
                "{changes:?}"
            );
            assert!(rendered(&changes).contains("[FROZEN BOOTSTRAP]"));
        }
    }

    /// One side holding `records` and nothing else.
    fn only(records: Vec<Value>) -> Vec<Value> {
        vec![json!({ "records": records })]
    }

    /// A record of `kind` that the comparison names under `key`.
    fn record_named(kind: RecordKind, key: &str) -> Value {
        match kind {
            RecordKind::Endpoint => {
                let (family, path) = key
                    .split_once(' ')
                    .expect("an endpoint identity is a family and a path");
                json!({
                    "delivery": "query",
                    "family": family,
                    "kind": "query",
                    "path": path,
                    "payload": Value::Null,
                    "record": "endpoint",
                    "request": {"fields": [], "kind": "struct"},
                    "response": {"fields": [], "kind": "struct"},
                })
            }
            RecordKind::Document => document("Doc", key),
            RecordKind::Envelope => json!({
                "body": {"fields": [], "kind": "struct"},
                "name": key,
                "record": "envelope",
            }),
            RecordKind::Identifier => identifier(key, "the frozen spelling"),
            RecordKind::Launch => launch("execution-id", true),
        }
    }

    /// Every entry in the frozen set is recognized when it breaks.
    ///
    /// The list is restated in this crate rather than imported, so the failure
    /// it can have is an entry naming an identity the comparison never
    /// produces: that entry would then silently protect nothing.
    #[test]
    fn every_frozen_record_is_recognized_when_it_breaks() {
        for (kind, key) in FROZEN_RECORDS {
            let record = record_named(kind, key);
            let changes = SurfaceSet::read(&only(vec![record]))
                .expect("the baseline reads")
                .changes_to(&SurfaceSet::read(&only(Vec::new())).expect("the side reads"));
            assert!(
                changes
                    .iter()
                    .any(RecordChange::breaks_the_frozen_bootstrap),
                "{} {key} is frozen but a break in it is not reported as one: {}",
                kind.token(),
                rendered(&changes)
            );
        }
    }

    /// The transport beneath the bootstrap is frozen on the same terms as the
    /// bootstrap itself: a client that cannot reach the key never decodes the
    /// one reply that would have named the disagreement.
    #[test]
    fn a_moved_transport_fact_beneath_the_bootstrap_is_a_frozen_finding() {
        let bus = |protocol: &str| {
            only(vec![
                identifier("bus-key-root", "phoxal/{execution}"),
                identifier("bus-key-composition", "phoxal/{execution}/{topic}"),
                identifier("encoding", "phoxal/v0;codec=1"),
                identifier("zenoh-wire-protocol", protocol),
            ])
        };
        let changes = SurfaceSet::read(&bus("9"))
            .expect("the baseline reads")
            .changes_to(&SurfaceSet::read(&bus("10")).expect("the current side reads"));
        let report = rendered(&changes);
        assert!(
            report.contains("changed  identifier zenoh-wire-protocol"),
            "{report}"
        );
        assert!(report.contains("[FROZEN BOOTSTRAP]"), "{report}");
        assert_eq!(
            CompatibilityImpact::of(&changes),
            CompatibilityImpact::Breaking
        );
    }

    /// The freeze is about the frozen record and nothing else: an ordinary
    /// break, and an addition beside the bootstrap, leave it where it was.
    #[test]
    fn changes_that_leave_the_bootstrap_alone_are_not_frozen_findings() {
        let unrelated = changes_between(
            vec![connect(), endpoint("robot/drive/state", "speed", "f64")],
            vec![connect()],
        );
        assert_eq!(
            CompatibilityImpact::of(&unrelated),
            CompatibilityImpact::Breaking
        );
        assert!(
            !unrelated
                .iter()
                .any(RecordChange::breaks_the_frozen_bootstrap)
        );

        let added = changes_between(
            vec![connect()],
            vec![connect(), endpoint("robot/drive/state", "speed", "f64")],
        );
        assert_eq!(
            CompatibilityImpact::of(&added),
            CompatibilityImpact::Additive
        );
        assert!(!added.iter().any(RecordChange::breaks_the_frozen_bootstrap));
        assert!(!rendered(&added).contains("FROZEN"));
    }
}
