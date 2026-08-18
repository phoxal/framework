//! The machine-readable statement of what one crate puts on a process
//! boundary.
//!
//! [`wire_schema`](crate::wire_schema) says what one *type* serializes to. This
//! module says which contracts a crate owns at all: which endpoints exist and
//! what each carries, which persisted documents it defines, which envelope
//! rides beside every sample, which wire constants a peer has to spell exactly,
//! and which arguments a supervised process is launched with.
//!
//! Every contract-owning crate exposes its own surface through a
//! `#[doc(hidden)] pub mod __compat` whose `contract_surface()` returns the
//! canonical rendering of one [`ContractSurface`]. A fact is emitted by the
//! crate that owns the declaration it is read from, so nothing here is a copied
//! list that could disagree with the code beside it.
//!
//! # Canonical rendering
//!
//! [`ContractSurface::canonical_json`] follows the same conventions as
//! [`WireSchema::canonical_json`]: sorted object keys, no whitespace, and
//! records held in a deterministic order rather than authoring order. Two
//! builds of an unchanged contract render byte-identical output, so a stored
//! baseline is compared with a plain string equality and a diff names the
//! record that moved.
//!
//! Records sort by a stable key that starts with the record's own kind, so a
//! new record kind cannot reorder the ones already published. The one order
//! that is *not* normalized is a launch argument list, which is held in the
//! order the parser declares.
//!
//! # Generator evolution
//!
//! Within one compatibility line, a change to this machinery must not alter the
//! rendered output for an unchanged contract. A refactor that moved bytes would
//! report a break that never happened in every baseline comparison at once.
//! Change the rendering only together with a deliberate re-baseline.

use crate::wire_schema::{WireSchema, render_list, render_string};

/// Everything one crate declares at a process boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractSurface {
    records: Vec<ContractRecord>,
}

impl ContractSurface {
    /// Collect one crate's records, normalized into their canonical order.
    #[must_use]
    pub fn new(records: impl IntoIterator<Item = ContractRecord>) -> Self {
        let mut records = records.into_iter().collect::<Vec<_>>();
        records.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
        Self { records }
    }

    /// The records, in canonical order.
    #[must_use]
    pub fn records(&self) -> &[ContractRecord] {
        &self.records
    }

    /// The canonical rendering: sorted keys, no whitespace, one document.
    #[must_use]
    pub fn canonical_json(&self) -> String {
        let mut out = String::from(r#"{"records":"#);
        render_list(&self.records, &mut out, ContractRecord::render);
        out.push('}');
        out
    }
}

/// One declared contract fact.
///
/// The variants are the shapes a Phoxal process boundary is actually made of.
/// A fact that fits none of them is not silently flattened into a neighbouring
/// variant: an owner who needs a new one adds it here on purpose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContractRecord {
    /// One bus endpoint: its key, its semantic kind and delivery family, and
    /// the wire shape of every body it carries.
    ///
    /// A pub/sub endpoint carries a `payload` and no request/response; a query
    /// endpoint carries a `request` and a `response` and no payload. Which pair
    /// is present is therefore the "query or topic" fact itself, rather than a
    /// second field that could disagree with it.
    Endpoint {
        /// The contract family, which is also the leading key segment.
        family: String,
        /// The family-rooted wire key template, with dynamic segments spelled
        /// as `{name}`.
        path: String,
        /// The fixed semantic endpoint kind, in the spelling the bus enum owns.
        kind: String,
        /// The transport family the kind selects, in the bus enum's spelling.
        delivery: String,
        /// The pub/sub body.
        payload: Option<WireSchema>,
        /// The query request body.
        request: Option<WireSchema>,
        /// The query response body.
        response: Option<WireSchema>,
    },
    /// One persisted or embedded document: its schema tag and its whole body.
    Document {
        /// The owning declaration's name, so a diff names the type to open.
        name: String,
        /// The document's own format tag, such as `phoxal/manifest/v0`.
        tag: String,
        /// The complete document shape, tag included.
        body: WireSchema,
    },
    /// One envelope that rides beside a body rather than inside it.
    Envelope {
        /// The owning declaration's name.
        name: String,
        /// The envelope's wire shape.
        body: WireSchema,
    },
    /// One exact wire constant a peer has to spell identically: an encoding
    /// string, a key root, a reserved prefix.
    Identifier {
        /// What the constant is, so a diff says which rule changed.
        name: String,
        /// The exact text that goes on the wire.
        value: String,
    },
    /// The argv contract a supervised process is launched with.
    Launch {
        /// The declared arguments, in the parser's own order.
        arguments: Vec<LaunchArgument>,
    },
}

impl ContractRecord {
    /// One pub/sub endpoint.
    #[must_use]
    pub fn topic(
        family: impl Into<String>,
        path: impl Into<String>,
        kind: impl Into<String>,
        delivery: impl Into<String>,
        payload: WireSchema,
    ) -> Self {
        Self::Endpoint {
            family: family.into(),
            path: path.into(),
            kind: kind.into(),
            delivery: delivery.into(),
            payload: Some(payload),
            request: None,
            response: None,
        }
    }

    /// One request/reply endpoint.
    #[must_use]
    pub fn query(
        family: impl Into<String>,
        path: impl Into<String>,
        kind: impl Into<String>,
        delivery: impl Into<String>,
        request: WireSchema,
        response: WireSchema,
    ) -> Self {
        Self::Endpoint {
            family: family.into(),
            path: path.into(),
            kind: kind.into(),
            delivery: delivery.into(),
            payload: None,
            request: Some(request),
            response: Some(response),
        }
    }

    /// One schema-tagged document.
    #[must_use]
    pub fn document(name: impl Into<String>, tag: impl Into<String>, body: WireSchema) -> Self {
        Self::Document {
            name: name.into(),
            tag: tag.into(),
            body,
        }
    }

    /// One out-of-body envelope.
    #[must_use]
    pub fn envelope(name: impl Into<String>, body: WireSchema) -> Self {
        Self::Envelope {
            name: name.into(),
            body,
        }
    }

    /// One exact wire constant.
    #[must_use]
    pub fn identifier(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self::Identifier {
            name: name.into(),
            value: value.into(),
        }
    }

    /// One launch contract, in the parser's declared argument order.
    #[must_use]
    pub fn launch(arguments: impl IntoIterator<Item = LaunchArgument>) -> Self {
        Self::Launch {
            arguments: arguments.into_iter().collect(),
        }
    }

    /// The key this record sorts on.
    ///
    /// It opens with the record's kind so adding a kind cannot reorder the
    /// records already published, and continues with the fields that identify
    /// one record within its kind.
    fn sort_key(&self) -> (&'static str, &str, &str) {
        match self {
            Self::Endpoint { family, path, .. } => ("endpoint", family, path),
            Self::Document { name, tag, .. } => ("document", name, tag),
            Self::Envelope { name, .. } => ("envelope", name, ""),
            Self::Identifier { name, .. } => ("identifier", name, ""),
            // One crate declares at most one launch contract, so the kind alone
            // places it.
            Self::Launch { .. } => ("launch", "", ""),
        }
    }

    fn render(&self, out: &mut String) {
        match self {
            Self::Endpoint {
                family,
                path,
                kind,
                delivery,
                payload,
                request,
                response,
            } => {
                out.push_str(r#"{"delivery":"#);
                render_string(delivery, out);
                out.push_str(r#","family":"#);
                render_string(family, out);
                out.push_str(r#","kind":"#);
                render_string(kind, out);
                out.push_str(r#","path":"#);
                render_string(path, out);
                out.push_str(r#","payload":"#);
                render_optional(payload.as_ref(), out);
                out.push_str(r#","record":"endpoint","request":"#);
                render_optional(request.as_ref(), out);
                out.push_str(r#","response":"#);
                render_optional(response.as_ref(), out);
                out.push('}');
            }
            Self::Document { name, tag, body } => {
                out.push_str(r#"{"body":"#);
                body.render(out);
                out.push_str(r#","name":"#);
                render_string(name, out);
                out.push_str(r#","record":"document","tag":"#);
                render_string(tag, out);
                out.push('}');
            }
            Self::Envelope { name, body } => {
                out.push_str(r#"{"body":"#);
                body.render(out);
                out.push_str(r#","name":"#);
                render_string(name, out);
                out.push_str(r#","record":"envelope"}"#);
            }
            Self::Identifier { name, value } => {
                out.push_str(r#"{"name":"#);
                render_string(name, out);
                out.push_str(r#","record":"identifier","value":"#);
                render_string(value, out);
                out.push('}');
            }
            Self::Launch { arguments } => {
                out.push_str(r#"{"arguments":"#);
                render_list(arguments, out, LaunchArgument::render);
                out.push_str(r#","record":"launch"}"#);
            }
        }
    }
}

/// One declared command-line argument of a launch contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchArgument {
    /// The long option spelling, without its leading dashes.
    pub name: String,
    /// Whether the process refuses to start without it.
    pub required: bool,
    /// Whether the option may be given more than once, accumulating values.
    pub repeated: bool,
    /// What the option consumes from argv.
    pub value: LaunchValueShape,
}

impl LaunchArgument {
    /// Declare one argument.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        required: bool,
        repeated: bool,
        value: LaunchValueShape,
    ) -> Self {
        Self {
            name: name.into(),
            required,
            repeated,
            value,
        }
    }

    fn render(&self, out: &mut String) {
        out.push_str(r#"{"name":"#);
        render_string(&self.name, out);
        out.push_str(r#","repeated":"#);
        out.push_str(if self.repeated { "true" } else { "false" });
        out.push_str(r#","required":"#);
        out.push_str(if self.required { "true" } else { "false" });
        out.push_str(r#","value":""#);
        out.push_str(self.value.token());
        out.push_str("\"}");
    }
}

/// What one launch argument consumes from argv.
///
/// The two shapes are what a parser actually distinguishes on the command line:
/// an option that stands alone, and one that consumes the argv token after it.
/// A value parser then narrows that token to an identity, a path, or a
/// duration, which is decode-side narrowing rather than shape - exactly as a
/// wire type's `try_from` narrows what decodes without changing what is
/// written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchValueShape {
    /// The option stands alone and consumes nothing.
    Flag,
    /// The option consumes one argv token.
    Text,
}

impl LaunchValueShape {
    const fn token(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Text => "text",
        }
    }
}

fn render_optional(schema: Option<&WireSchema>, out: &mut String) {
    match schema {
        Some(schema) => schema.render(out),
        None => out.push_str("null"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire_schema::{WireField, WireSchema};

    fn body() -> WireSchema {
        WireSchema::structure([WireField::required("value", WireSchema::U8)])
    }

    /// The canonical rendering is a function of the record set alone, so the
    /// order an owner happened to declare its records in cannot move a byte.
    #[test]
    fn authoring_order_does_not_change_the_canonical_bytes() {
        let declared = [
            ContractRecord::identifier("encoding", "phoxal/v0;codec=1"),
            ContractRecord::topic("robot", "robot/drive/state", "state", "state", body()),
            ContractRecord::document("Doc", "phoxal/example/v0", body()),
        ];
        let mut reversed = declared.clone();
        reversed.reverse();
        assert_eq!(
            ContractSurface::new(declared.clone()).canonical_json(),
            ContractSurface::new(reversed).canonical_json()
        );
        assert_eq!(
            ContractSurface::new(declared.clone()),
            ContractSurface::new(declared)
        );
    }

    /// Records group by kind first, so adding a kind cannot reorder the records
    /// a published baseline already holds.
    #[test]
    fn records_sort_by_kind_and_then_by_identity() {
        let surface = ContractSurface::new([
            ContractRecord::launch([LaunchArgument::new(
                "only",
                true,
                false,
                LaunchValueShape::Text,
            )]),
            ContractRecord::identifier("root", "phoxal"),
            ContractRecord::topic("robot", "robot/b", "state", "state", body()),
            ContractRecord::topic("robot", "robot/a", "state", "state", body()),
            ContractRecord::envelope("Envelope", body()),
            ContractRecord::document("Doc", "phoxal/example/v0", body()),
        ]);
        let kinds = surface
            .records()
            .iter()
            .map(ContractRecord::sort_key)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                ("document", "Doc", "phoxal/example/v0"),
                ("endpoint", "robot", "robot/a"),
                ("endpoint", "robot", "robot/b"),
                ("envelope", "Envelope", ""),
                ("identifier", "root", ""),
                ("launch", "", ""),
            ]
        );
    }

    /// Every record renders as whitespace-free JSON whose keys ascend, so a
    /// reviewer reading a baseline diff sees a contract change and never a
    /// formatting artifact.
    #[test]
    fn every_record_renders_as_canonical_json() {
        let surface = ContractSurface::new([
            ContractRecord::topic("robot", "robot/drive/state", "state", "state", body()),
            ContractRecord::query(
                "supervisor",
                "supervisor/connect",
                "query",
                "query",
                body(),
                body(),
            ),
            ContractRecord::document("Doc", "phoxal/example/v0", body()),
            ContractRecord::envelope("Envelope", body()),
            ContractRecord::identifier("encoding", "phoxal/v0;codec=1"),
            ContractRecord::launch([
                LaunchArgument::new("execution-id", true, false, LaunchValueShape::Text),
                LaunchArgument::new("verbose", false, false, LaunchValueShape::Flag),
            ]),
        ]);
        let rendered = surface.canonical_json();
        assert!(!rendered.contains(' '), "{rendered}");
        let parsed = serde_json::from_str::<serde_json::Value>(&rendered)
            .expect("the canonical rendering is JSON");
        assert_eq!(
            parsed["records"]
                .as_array()
                .map(|records| records.len())
                .unwrap_or_default(),
            6
        );
        // A second rendering of the same surface is the same bytes.
        assert_eq!(surface.canonical_json(), rendered);
    }

    /// A pub/sub record carries a payload and no request pair; a query record
    /// carries the pair and no payload. The absent half is written as an
    /// explicit `null`, so a baseline records which kind of endpoint it was.
    #[test]
    fn an_endpoint_record_states_whether_it_is_a_topic_or_a_query() {
        let topic = ContractSurface::new([ContractRecord::topic(
            "robot",
            "robot/drive/state",
            "state",
            "state",
            body(),
        )])
        .canonical_json();
        assert!(
            topic.contains(r#""request":null,"response":null"#),
            "{topic}"
        );

        let query = ContractSurface::new([ContractRecord::query(
            "robot",
            "robot/frame/lookup",
            "query",
            "query",
            body(),
            body(),
        )])
        .canonical_json();
        assert!(query.contains(r#""payload":null"#), "{query}");
    }

    /// The order a parser declares its arguments in is the order a reviewer
    /// reads a `--help` in, so it survives rather than being sorted away.
    #[test]
    fn launch_arguments_keep_the_parsers_declared_order() {
        let rendered = ContractSurface::new([ContractRecord::launch([
            LaunchArgument::new("zulu", true, false, LaunchValueShape::Text),
            LaunchArgument::new("alpha", false, true, LaunchValueShape::Text),
        ])])
        .canonical_json();
        let zulu = rendered.find("zulu").expect("the first argument renders");
        let alpha = rendered.find("alpha").expect("the second argument renders");
        assert!(zulu < alpha, "{rendered}");
    }

    #[test]
    fn a_name_with_json_metacharacters_is_escaped() {
        let rendered = ContractSurface::new([ContractRecord::identifier("quoted", "a\"b\\c")])
            .canonical_json();
        assert!(rendered.contains(r#""a\"b\\c""#), "{rendered}");
        serde_json::from_str::<serde_json::Value>(&rendered).expect("still valid JSON");
    }
}
