//! Graph validation for Phoxal participant graphs (D59/D63).
//!
//! This is the pure core: given the `emit-apis` report of every participant in a
//! robot graph, it enforces the one thing that decides whether framework and
//! user artifacts speak the same world: per-contract `schema_id` agreement,
//! grouped by `family` alone. It is deliberately independent of how the reports
//! are obtained (resolved images, local binaries), so it is fully unit-testable
//! without Docker or a registry.
//!
//! **Per-contract wire-shape agreement (#16-b).** Every participant that uses a
//! given contract `family` must report the same `schema_id` (the framework's
//! normalized transitive wire-shape hash). Mixed `api_version`s are allowed as
//! long as the shared family's `schema_id`s agree, because the bus decode
//! fast-rejects on `schema_id`, not on `api_version`. A `family` used with more
//! than one distinct `schema_id` across its participants is a hard mismatch
//! reported against that family. This applies uniformly to every participant,
//! with no exemptions, and there is no enforcement of `api::internal::*` vs
//! `api::*` - there is none today and none is added here.
//!
//! Nothing about pub/sub topology, query responder counts, or topic shape is
//! checked here. A robot legitimately consumes commands whose sender is
//! external to the checked participant set (an operator UI, a joystick, a sim
//! controller), and legitimately offers query endpoints that nothing on-robot
//! currently calls (the callers are external tools). The bus already makes
//! these non-issues at runtime: a publisher checks for subscribers before
//! sending, and a query server starts regardless of current clients. So a
//! consumed contract with no producer and a produced contract with no consumer
//! are both legal states, not problems - this checker does not track them.
//!
//! Simulation plans differ from deploy/run plans only in *which participants the
//! caller passes*: in sim, a component driver is simply not launched and the
//! Webots simulator participant is passed in its place (D16). Because the
//! simulator is built from the same framework, it speaks the same contracts by
//! construction, so the only thing that matters is `schema_id` agreement, which
//! the rule above already enforces uniformly. There is no separate substitution
//! concept, no completeness gate, and no missing-producer diagnostic here -
//! whether a contract has a producer is a caller/deployment choice, not something
//! this checker judges.
use std::collections::{BTreeMap, BTreeSet};

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
    /// by the checker itself - schema agreement applies to every participant
    /// uniformly; preserved for diagnostics.
    pub participant_class: ParticipantClass,
    /// The API version the artifact reports (`emit-apis` `api_version`).
    pub api_version: String,
    /// The framework-owned bus ABI reported by the artifact, if present.
    pub bus_abi: Option<String>,
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

/// One `{family, schema_id}` contract use from an `emit-apis` report.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Contract {
    pub family: String,
    /// The framework's normalized transitive wire-shape hash for this contract
    /// body (`emit-apis` per-contract `schema_id`). Participants sharing a
    /// `family` must agree on this exact id; the bus decode fast-rejects on a
    /// `schema_id` mismatch.
    pub schema_id: String,
}

/// Borrowed input to the pure graph checker.
#[derive(Debug, Clone, Copy)]
pub struct CheckInput<'a> {
    pub participants: &'a [ParticipantApis],
}

/// A problem found while validating a robot graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// Participants sharing a contract `family` disagree on its `schema_id`
    /// (the normalized transitive wire-shape hash). Because the bus decode
    /// fast-rejects on `schema_id`, a shared family with more than one distinct
    /// `schema_id` can never interoperate.
    ContractSchemaMismatch {
        family: String,
        /// Each disagreeing `schema_id` paired with the sorted participant ids
        /// that report it. Sorted by `schema_id` so output is deterministic.
        schema_ids: Vec<(String, Vec<String>)>,
    },
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

/// Validate a robot graph: per-contract `schema_id` agreement (#16-b), grouped
/// by `family` alone.
///
/// `participants` is every normal participant's `emit-apis` report. Problems are
/// returned in a stable order (schema mismatches by family) so output and tests
/// are deterministic.
#[must_use]
pub fn check_graph(participants: &[ParticipantApis]) -> Report {
    check_plan(CheckInput { participants })
}

#[must_use]
pub fn check_plan(input: CheckInput<'_>) -> Report {
    // Per-contract wire-shape agreement (#16-b). Group contracts by `family`,
    // then by `schema_id`; a shared family used with more than one distinct
    // `schema_id` across its participants can never interoperate (the bus
    // decode fast-rejects on `schema_id`), so it is a hard mismatch. Mixed
    // `api_version`s are allowed as long as a shared family's `schema_id`s
    // agree. Applies to every participant, with no exemptions.
    Report {
        problems: schema_mismatches(input.participants),
    }
}

/// Per-contract `schema_id` agreement (#16-b).
///
/// Group contracts by `family`, then by `schema_id`, recording the participants
/// reporting each id. A `family` used with more than one distinct `schema_id`
/// across its participants can never interoperate, so it is reported as a
/// `ContractSchemaMismatch`.
fn schema_mismatches(participants: &[ParticipantApis]) -> Vec<Problem> {
    let mut by_family: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    for p in participants {
        for c in &p.contracts {
            by_family
                .entry(c.family.clone())
                .or_default()
                .entry(c.schema_id.clone())
                .or_default()
                .insert(p.participant_id.clone());
        }
    }

    by_family
        .into_iter()
        .filter(|(_, schema_ids)| schema_ids.len() > 1)
        .map(|(family, schema_ids)| Problem::ContractSchemaMismatch {
            family,
            schema_ids: schema_ids
                .into_iter()
                .map(|(schema_id, participants)| (schema_id, participants.into_iter().collect()))
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(family: &str) -> Contract {
        contract_with_schema(family, "deadbeef")
    }

    fn contract_with_schema(family: &str, schema_id: &str) -> Contract {
        Contract {
            family: family.to_string(),
            schema_id: schema_id.to_string(),
        }
    }

    fn participant(id: &str, api: &str, contracts: Vec<Contract>) -> ParticipantApis {
        ParticipantApis {
            participant_id: id.to_string(),
            artifact_id: id.to_string(),
            participant_kind: ParticipantKind::Service,
            participant_class: ParticipantClass::Checked,
            api_version: api.to_string(),
            bus_abi: None,
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
            bus_abi: None,
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
            participant("mission", "y2026_1", vec![contract("drive::Target")]),
            participant("drive", "y2026_1", vec![contract("drive::Target")]),
        ];
        assert!(check_graph(&graph).is_ok());
    }

    #[test]
    fn healthy_query_graph_has_no_problems() {
        // a server serves asset/get; a client queries it.
        let graph = vec![
            participant(
                "asset",
                "y2026_1",
                vec![
                    contract("asset::GetRequest"),
                    contract("asset::GetResponse"),
                ],
            ),
            participant(
                "client",
                "y2026_1",
                vec![
                    contract("asset::GetRequest"),
                    contract("asset::GetResponse"),
                ],
            ),
        ];
        assert!(check_graph(&graph).is_ok());
    }

    #[test]
    fn contract_schema_mismatch_is_reported_per_family() {
        // Two participants share a `family` contract but report DIFFERENT
        // `schema_id`s -> one `ContractSchemaMismatch` naming each disagreeing id
        // and who reports it. Mixed api_versions on their own are allowed.
        let graph = vec![
            participant(
                "mission",
                "y2026_1",
                vec![contract_with_schema("drive::Target", "aaaa")],
            ),
            participant(
                "drive",
                "y2026_2",
                vec![contract_with_schema("drive::Target", "bbbb")],
            ),
        ];
        let report = check_graph(&graph);
        assert_eq!(
            report.problems,
            vec![Problem::ContractSchemaMismatch {
                family: "drive::Target".to_string(),
                schema_ids: vec![
                    ("aaaa".to_string(), vec!["mission".to_string()]),
                    ("bbbb".to_string(), vec!["drive".to_string()]),
                ],
            }]
        );
    }

    #[test]
    fn matching_contract_schema_ids_pass_even_with_mixed_api_versions() {
        // Same `family` and same `schema_id` on both sides -> no problem, even
        // though the two participants report different api_versions.
        let graph = vec![
            participant(
                "mission",
                "y2026_1",
                vec![contract_with_schema("drive::Target", "cafe")],
            ),
            participant(
                "drive",
                "y2026_2",
                vec![contract_with_schema("drive::Target", "cafe")],
            ),
        ];
        assert!(check_graph(&graph).is_ok());
    }

    #[test]
    fn tool_kind_participant_contracts_still_participate_in_schema_agreement() {
        let graph = vec![
            participant(
                "mission",
                "y2026_1",
                vec![contract_with_schema("drive::Target", "aaaa")],
            ),
            participant(
                "drive",
                "y2026_1",
                vec![contract_with_schema("drive::Target", "aaaa")],
            ),
            privileged_participant(
                "inspector",
                "y2026_1",
                vec![contract_with_schema("drive::Target", "bbbb")],
            ),
        ];

        let report = check_graph(&graph);

        assert_eq!(
            report.problems,
            vec![Problem::ContractSchemaMismatch {
                family: "drive::Target".to_string(),
                schema_ids: vec![
                    (
                        "aaaa".to_string(),
                        vec!["drive".to_string(), "mission".to_string()]
                    ),
                    ("bbbb".to_string(), vec!["inspector".to_string()]),
                ],
            }]
        );
    }

    #[test]
    fn a_publisher_anywhere_satisfies_all_subscribers() {
        let graph = vec![
            participant("odometry", "y2026_1", vec![contract("odometry::State")]),
            participant("localize", "y2026_1", vec![contract("odometry::State")]),
            participant("map", "y2026_1", vec![contract("odometry::State")]),
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
            participant("mission", "y2026_1", vec![contract("mission::Command")]),
            // Offers a query endpoint with no client in the graph.
            participant(
                "asset",
                "y2026_1",
                vec![
                    contract("asset::GetRequest"),
                    contract("asset::GetResponse"),
                ],
            ),
        ];

        let report = check_graph(&participants);

        assert_eq!(report, Report::default());
    }

    #[test]
    fn empty_graph_is_ok() {
        assert!(check_graph(&[]).is_ok());
    }
}
