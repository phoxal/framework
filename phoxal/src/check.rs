//! Graph validation for Phoxal participant graphs (D59/D63/D1).
//!
//! This is the pure core: given the `emit-apis` report of every participant in a
//! robot graph, it is fully unit-testable without Docker or a registry,
//! independent of how the reports are obtained (resolved images, local
//! binaries).
//!
//! **There is no interop gate on contract identity (D1).** Contract identity is
//! the version-qualified `family` name alone (e.g. `"api::drive::Target"`);
//! there is no `schema_id` hash to agree on, because two participants naming the
//! same version-qualified contract are compatible by construction (same name
//! ⇒ same frozen shape, enforced by the type system and made physically real by
//! the version-qualified wire key). Two participants naming *different*
//! contracts are simply different topics, never a collision. `check_plan`/
//! `check_graph` below therefore carry only [`Problem::InvalidConfig`], the one
//! thing that *is* still a real runtime hazard on that axis: a user runtime's
//! manifest config not matching its emitted JSON Schema.
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
//! There is no API-coherence pass and no contract-surface inventory
//! (organization#957). The exact framework train, plus the train-selected
//! `phoxal::api` facade, is the entire API compatibility boundary: every
//! participant on one robot is compiled against one train, so a version
//! disagreement between two of them is not expressible. Missing publishers and
//! unserved queries are project composition and runtime behaviour, not a
//! release-time compatibility problem.
//!
//! Simulation plans differ from deploy/run plans only in *which participants the
//! caller passes*: in sim, a component driver is simply not launched and the
//! Webots simulator participant is passed in its place (D16). Because the
//! simulator is built from the same framework, it speaks the same contracts by
//! construction. There is no separate substitution concept, no completeness
//! gate, and no missing-producer diagnostic here - whether a contract has a
//! producer is a caller/deployment choice, not something this checker judges.

/// One participant's `emit-apis` report, reduced to what graph validation needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantApis {
    /// The concrete participant/instance id used for graph membership and
    /// diagnostics. For most participants this equals `artifact_id`, but a
    /// component driver is launched once per component instance, so several
    /// instances of the same driver share one `artifact_id` yet must remain
    /// distinct nodes in the graph (e.g. `left_drive`, `right_drive`).
    pub participant_id: String,
    /// The artifact id (`emit-apis` `artifact.id`), e.g. `"drive"`. Kept for
    /// artifact-identity validation; not used to key the topology graph.
    pub artifact_id: String,
    /// The artifact kind (`emit-apis` `artifact.kind`), e.g. `"service"`,
    /// `"driver"`, or `"simulator"`. Not consulted by the checker itself;
    /// preserved for callers (e.g. board display of which driver a sim plan's
    /// simulator participant stands in for).
    pub participant_kind: ParticipantKind,
    /// The `emit-apis` `participant_class` the artifact reports. Not consulted
    /// by the checker itself - name identity applies to every participant
    /// uniformly; preserved for diagnostics.
    pub participant_class: ParticipantClass,
    /// The API version the artifact reports (`emit-apis` `api_version`).
    pub api_version: String,
    /// The artifact's emitted config schema, preserved for later validation.
    pub config_schema: Option<serde_json::Value>,
    /// The manifest scope this participant is launched under. Normal runtimes
    /// see the whole graph; component drivers are launched once per component
    /// instance.
    pub scope: ParticipantScope,
    /// The contracts the artifact participates in.
    pub contracts: Vec<Contract>,
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
    /// Parse the `emit-apis` `artifact.kind` string. Unknown kinds are preserved
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
    /// Parse the `emit-apis` `participant_class` string.
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

/// One contract use from an `emit-apis` report: its version-qualified name
/// (e.g. `"api::drive::Target"`, D1). There is no `schema_id` - the name
/// itself is the whole identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Contract {
    pub family: String,
}

/// Borrowed input to the pure graph checker.
#[derive(Debug, Clone, Copy)]
pub struct CheckInput<'a> {
    pub participants: &'a [ParticipantApis],
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
/// `participants` is every normal participant's `emit-apis` report. There is no
/// contract-agreement axis left to check (D1: name identity alone guarantees
/// compatibility) - this is a thin, stable entry point for callers, kept for
/// config validation to grow into.
#[must_use]
pub fn check_graph(participants: &[ParticipantApis]) -> Report {
    check_plan(CheckInput { participants })
}

#[must_use]
pub fn check_plan(input: CheckInput<'_>) -> Report {
    let _ = input.participants;
    Report::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(family: &str) -> Contract {
        Contract {
            family: family.to_string(),
        }
    }

    fn participant(id: &str, api: &str, contracts: Vec<Contract>) -> ParticipantApis {
        ParticipantApis {
            participant_id: id.to_string(),
            artifact_id: id.to_string(),
            participant_kind: ParticipantKind::Service,
            participant_class: ParticipantClass::Checked,
            api_version: api.to_string(),
            config_schema: None,
            scope: ParticipantScope::Graph,
            contracts,
        }
    }

    fn privileged_participant(id: &str, api: &str, contracts: Vec<Contract>) -> ParticipantApis {
        ParticipantApis {
            participant_id: id.to_string(),
            artifact_id: id.to_string(),
            participant_kind: ParticipantKind::Tool,
            participant_class: ParticipantClass::Privileged,
            api_version: api.to_string(),
            config_schema: None,
            scope: ParticipantScope::Graph,
            contracts,
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
    fn healthy_pubsub_graph_has_no_problems() {
        // producer publishes drive/target; consumer subscribes it.
        let graph = vec![
            participant("mission", "v1", vec![contract("api::drive::Target")]),
            participant("drive", "v1", vec![contract("api::drive::Target")]),
        ];
        assert!(check_graph(&graph).is_ok());
    }

    #[test]
    fn healthy_query_graph_has_no_problems() {
        // a server serves asset/get; a client queries it.
        let graph = vec![
            participant("asset", "v1", vec![contract("api::asset::GetRequest")]),
            participant("client", "v1", vec![contract("api::asset::GetRequest")]),
        ];
        assert!(check_graph(&graph).is_ok());
    }

    #[test]
    fn mixed_versions_on_the_same_contract_family_are_simply_different_contracts() {
        // D1: with no schema_id, a `api::drive::Target` user and a
        // `api::drive::Target` user are unrelated contracts, not a
        // mismatch - there is nothing to report.
        let graph = vec![
            participant("mission", "v1", vec![contract("api::drive::Target")]),
            participant("drive", "v2", vec![contract("api::drive::Target")]),
        ];
        assert!(check_graph(&graph).is_ok());
    }

    #[test]
    fn tool_kind_participant_contracts_do_not_gate_the_graph() {
        let graph = vec![
            participant("mission", "v1", vec![contract("api::drive::Target")]),
            participant("drive", "v1", vec![contract("api::drive::Target")]),
            privileged_participant("inspector", "v1", vec![contract("api::drive::Target")]),
        ];

        assert!(check_graph(&graph).is_ok());
    }

    #[test]
    fn a_publisher_anywhere_satisfies_all_subscribers() {
        let graph = vec![
            participant("odometry", "v1", vec![contract("api::odometry::State")]),
            participant("localize", "v1", vec![contract("api::odometry::State")]),
            participant("map", "v1", vec![contract("api::odometry::State")]),
        ];
        assert!(check_graph(&graph).is_ok());
    }

    #[test]
    fn dangling_consumers_and_dangling_servers_are_legal() {
        // A real robot: a consumed command with no producer in the checked set
        // (an external operator/joystick/sim-controller sends it), and a query
        // server with no current client (the caller is an external tool). Neither
        // is a problem or warning; the bus tolerates both at runtime.
        let participants = vec![
            // Consumes a command nobody in the graph produces.
            participant(
                "navigation",
                "v1",
                vec![contract("api::navigation::Request")],
            ),
            // Offers a query endpoint with no client in the graph.
            participant("asset", "v1", vec![contract("api::asset::GetRequest")]),
        ];

        let report = check_graph(&participants);

        assert_eq!(report, Report::default());
    }

    #[test]
    fn empty_graph_is_ok() {
        assert!(check_graph(&[]).is_ok());
    }

    // -----------------------------------------------------------------
}
