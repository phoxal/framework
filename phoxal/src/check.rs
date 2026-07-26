//! Participant classification and graph validation for Phoxal robot graphs
//! (D59/D63/D1).
//!
//! This is the pure core: given a reduced report of every participant in a
//! robot graph (`phoxal-cli` resolves those reports from images or local
//! binaries and this module never depends on how), it is fully unit-testable
//! without Docker or a registry. [`ParticipantKind`], [`ParticipantClass`], and
//! [`ParticipantScope`] are the classification vocabulary callers key graph
//! wiring on - which participant stands in for which during simulation
//! substitution, and which participants share a component-instance scope; they
//! are not themselves gated here.
//!
//! **There is no interop gate on contract identity (D1).** Two participants
//! naming the same version-qualified contract are compatible by construction
//! (same name => same frozen shape, enforced by the type system and made
//! physically real by the version-qualified wire key); two participants naming
//! *different* contracts are simply different topics, never a collision.
//! There is no API-coherence pass and no contract-surface inventory
//! (organization#957): the exact framework train, plus the train-selected
//! `phoxal::api` facade, is the entire API compatibility boundary, so a
//! version disagreement between two participants on one robot is not
//! expressible. [`check_graph`] therefore reports only [`Problem::InvalidConfig`],
//! the one thing that *is* still a real runtime hazard: a user runtime's
//! manifest config not matching its emitted JSON Schema. Constructing that
//! problem is the caller's job (config-schema validation lives with the
//! caller that resolved the schema); this module carries the vehicle, not the
//! validator.
//!
//! Nothing about pub/sub topology or query responder counts is checked here. A
//! robot legitimately consumes commands whose sender is external to the checked
//! participant set (an operator UI, a joystick, a sim controller), and
//! legitimately offers query endpoints that nothing on-robot currently calls
//! (the callers are external tools). The bus already makes these non-issues at
//! runtime: a publisher checks for subscribers before sending, and a query
//! server starts regardless of current clients. So a consumed contract with no
//! producer and a produced contract with no consumer are both legal states -
//! **that open-world stance stays, and is now the whole stance.**
//!
//! Simulation plans differ from deploy/run plans only in *which participants the
//! caller passes*: in sim, a component driver is simply not launched and the
//! Webots simulator participant is passed in its place (D16, [`ParticipantKind::is_simulator`]).
//! Because the simulator is built from the same framework, it speaks the same
//! contracts by construction. There is no separate substitution concept, no
//! completeness gate, and no missing-producer diagnostic here - whether a
//! contract has a producer is a caller/deployment choice, not something this
//! checker judges.

/// One participant's reduced report, carrying what graph classification and
/// config validation need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantApis {
    /// The concrete participant/instance id used for graph membership and
    /// diagnostics. For most participants this equals `artifact_id`, but a
    /// component driver is launched once per component instance, so several
    /// instances of the same driver share one `artifact_id` yet must remain
    /// distinct nodes in the graph (e.g. `left_drive`, `right_drive`).
    pub participant_id: String,
    /// The artifact id (e.g. `"drive"`). Kept for artifact-identity
    /// validation; not used to key the topology graph.
    pub artifact_id: String,
    /// The artifact kind, e.g. `"service"`, `"driver"`, or `"simulator"`. Not
    /// consulted by the checker itself; preserved for callers (e.g. board
    /// display of which driver a sim plan's simulator participant stands in
    /// for).
    pub participant_kind: ParticipantKind,
    /// The reported participant class. Not consulted by the checker itself -
    /// name identity applies to every participant uniformly; preserved for
    /// diagnostics.
    pub participant_class: ParticipantClass,
    /// The artifact's emitted config schema, preserved for later validation.
    pub config_schema: Option<serde_json::Value>,
    /// The manifest scope this participant is launched under. Normal runtimes
    /// see the whole graph; component drivers are launched once per component
    /// instance.
    pub scope: ParticipantScope,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ParticipantKind {
    Service,
    Driver,
    Tool,
    Simulator,
    Other(String),
}

impl ParticipantKind {
    /// Parse a reported `artifact.kind` string. Unknown kinds are preserved
    /// for diagnostics.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "service" => Self::Service,
            "driver" => Self::Driver,
            "tool" => Self::Tool,
            "simulator" => Self::Simulator,
            other => Self::Other(other.to_string()),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Service => "service",
            Self::Driver => "driver",
            Self::Tool => "tool",
            Self::Simulator => "simulator",
            Self::Other(kind) => kind,
        }
    }

    #[must_use]
    pub const fn is_simulator(&self) -> bool {
        matches!(self, Self::Simulator)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParticipantClass {
    #[default]
    Checked,
    Privileged,
}

impl ParticipantClass {
    /// Parse a reported `participant_class` string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "checked" => Self::Checked,
            "privileged" => Self::Privileged,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn is_checked(self) -> bool {
        matches!(self, Self::Checked)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ParticipantScope {
    #[default]
    Graph,
    ComponentInstance(String),
}

/// A problem found while validating a robot graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// A user runtime's manifest config does not match its emitted JSON Schema.
    InvalidConfig {
        runtime_id: String,
        errors: Vec<String>,
    },
}

/// The outcome of validating a graph: the problems found (empty == healthy).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub problems: Vec<Problem>,
}

impl Report {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Validate a robot graph.
///
/// `participants` is every normal participant's reduced report. There is no
/// contract-agreement axis left to check (D1: name identity alone guarantees
/// compatibility) - this is a thin, stable entry point for callers, kept for
/// config validation to grow into. It does not itself resolve or validate
/// `config_schema` against a manifest; a caller that has both in hand
/// constructs [`Problem::InvalidConfig`] and this stays the shared report
/// vehicle both sides agree on.
#[must_use]
pub fn check_graph(participants: &[ParticipantApis]) -> Report {
    let _ = participants;
    Report::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn participant(id: &str) -> ParticipantApis {
        ParticipantApis {
            participant_id: id.to_string(),
            artifact_id: id.to_string(),
            participant_kind: ParticipantKind::Service,
            participant_class: ParticipantClass::Checked,
            config_schema: None,
            scope: ParticipantScope::Graph,
        }
    }

    fn privileged_participant(id: &str) -> ParticipantApis {
        ParticipantApis {
            participant_id: id.to_string(),
            artifact_id: id.to_string(),
            participant_kind: ParticipantKind::Tool,
            participant_class: ParticipantClass::Privileged,
            config_schema: None,
            scope: ParticipantScope::Graph,
        }
    }

    #[test]
    fn participant_kind_parse_preserves_unknown_kinds() {
        assert_eq!(ParticipantKind::parse("service"), ParticipantKind::Service);
        assert_eq!(ParticipantKind::parse("driver"), ParticipantKind::Driver);
        assert_eq!(
            ParticipantKind::parse("simulator"),
            ParticipantKind::Simulator
        );
        assert_eq!(
            ParticipantKind::parse("custom-kind"),
            ParticipantKind::Other("custom-kind".to_string())
        );
    }

    #[test]
    fn participant_class_parse_round_trips_and_rejects_unknown() {
        assert_eq!(
            ParticipantClass::parse("checked"),
            Some(ParticipantClass::Checked)
        );
        assert_eq!(
            ParticipantClass::parse("privileged"),
            Some(ParticipantClass::Privileged)
        );
        assert_eq!(ParticipantClass::parse("service"), None);
    }

    #[test]
    fn healthy_graph_has_no_problems() {
        let graph = vec![participant("mission"), participant("drive")];
        assert!(check_graph(&graph).is_ok());
    }

    #[test]
    fn tool_kind_participants_do_not_gate_the_graph() {
        let graph = vec![
            participant("mission"),
            participant("drive"),
            privileged_participant("inspector"),
        ];

        assert!(check_graph(&graph).is_ok());
    }

    #[test]
    fn empty_graph_is_ok() {
        assert!(check_graph(&[]).is_ok());
    }
}
