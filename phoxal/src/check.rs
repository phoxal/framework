//! Graph validation for Phoxal participant graphs (D59/D63).
//!
//! This is the pure core: given the `emit-apis` report of every participant in a
//! robot graph, it enforces per-contract wire-shape agreement and the one
//! remaining topology rule. It is deliberately independent of how the reports are
//! obtained (resolved images, local binaries), so it is fully unit-testable
//! without Docker or a registry.
//!
//! 1. **Per-contract wire-shape agreement (#16-b).** Every participant that shares
//!    a `(family, topic)` contract must report the same `schema_id` (the framework's
//!    normalized transitive wire-shape hash). Mixed `api_version`s are allowed as
//!    long as the shared contracts' `schema_id`s agree, because the bus decode
//!    fast-rejects on `schema_id`, not on `api_version`. A `(family, topic)` used
//!    with more than one distinct `schema_id` across its participants is a hard
//!    mismatch reported against that contract. This applies uniformly to every
//!    participant, with no exemptions.
//! 2. **At most one query-server responder.** A query topic must have at most one
//!    server responder in the graph, because the bus expects exactly one
//!    responder for a query; more than one is a real conflict.
//!
//! Existence of the *other side* of a contract is deliberately not checked. A
//! robot legitimately consumes commands whose sender is external to the checked
//! participant set (an operator UI, a joystick, a sim controller), and legitimately
//! offers query endpoints that nothing on-robot currently calls (the callers are
//! external tools). A component-capability template that expands to zero concrete
//! instances is likewise legal - it simply contributes no concrete contracts to
//! the graph, because the robot may lack that optional component. The bus already
//! makes these non-issues at runtime: a publisher checks for subscribers before
//! sending, and a query server starts regardless of current clients. So a consumed
//! contract with no producer, a produced contract with no consumer, and an
//! unexpanded (zero-match) component template are all legal states, not problems.
//!
//! Simulation plans can also substitute a component driver's contract uses with
//! a simulator participant. Substitution is explicit plan input, legal only in
//! [`PlanMode::Sim`], and reported back on success so callers never treat a
//! missing driver as silently satisfied.
//!
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
    /// The artifact kind (`emit-apis` `artifact.kind`). Simulation substitutions
    /// require a checked participant of kind `simulator`; this is intentionally
    /// keyed on the framework kind, not on a concrete simulator implementation.
    pub participant_kind: ParticipantKind,
    /// The `emit-apis` `participant_class` the artifact reports. No longer used
    /// to gate schema agreement or the responder-conflict check (both apply to
    /// every participant uniformly); still consulted by sim substitution, which
    /// requires a checked provider.
    // TODO(followup): participant_class is now unused by the topology checks;
    // consider removing it from emit-apis + the macro once substitution no
    // longer needs it either.
    pub participant_class: ParticipantClass,
    /// The API version the artifact reports (`emit-apis` `api_version`).
    pub api_version: String,
    /// The framework-owned bus ABI reported by the artifact, if present.
    pub bus_abi: Option<String>,
    /// The artifact's emitted config schema, preserved for later validation.
    pub config_schema: Option<serde_json::Value>,
    /// The manifest scope this participant is launched under. Normal runtimes
    /// see the whole graph; component drivers are launched once per component
    /// instance and dynamic component topics must be materialized only for that
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
    /// for diagnostics, but only `simulator` can provide substitutions.
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

/// One `{family, topic, direction}` contract use from an `emit-apis` report.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Contract {
    pub family: String,
    pub topic: String,
    pub direction: Direction,
    /// The framework's normalized transitive wire-shape hash for this contract
    /// body (`emit-apis` per-contract `schema_id`). Participants sharing a
    /// `(family, topic)` must agree on this exact id; the bus decode fast-rejects
    /// on a `schema_id` mismatch.
    pub schema_id: String,
}

/// The direction a participant uses a contract on a topic. Mirrors the framework's
/// `emit-apis` `direction` vocabulary (snake_case on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Direction {
    Publish,
    Subscribe,
    QueryRequest,
    QueryResponse,
    ServerRequest,
    ServerResponse,
}

impl Direction {
    /// Parse the snake_case `emit-apis` direction string. Unknown strings return
    /// `None` so the caller can surface a forward-incompatible report rather than
    /// silently misclassifying it.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "publish" => Self::Publish,
            "subscribe" => Self::Subscribe,
            "query_request" => Self::QueryRequest,
            "query_response" => Self::QueryResponse,
            "server_request" => Self::ServerRequest,
            "server_response" => Self::ServerResponse,
            _ => return None,
        })
    }
}

/// Which launch plan the shared checker is validating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum PlanMode {
    #[default]
    Deploy,
    Run,
    Sim,
}

impl PlanMode {
    /// Parse the CLI-facing mode vocabulary.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "deploy" => Self::Deploy,
            "run" => Self::Run,
            "sim" => Self::Sim,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deploy => "deploy",
            Self::Run => "run",
            Self::Sim => "sim",
        }
    }
}

/// A sim-only substitution record built by the resolver from a component
/// driver's metadata. The contracts are the driver-side uses for one concrete
/// component instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractSubstitution {
    pub component_instance: String,
    pub provider_participant_id: String,
    pub contracts: Vec<Contract>,
}

/// Borrowed input to the pure graph checker.
#[derive(Debug, Clone, Copy)]
pub struct CheckInput<'a> {
    pub mode: PlanMode,
    pub participants: &'a [ParticipantApis],
    pub robot_graph: &'a RobotGraph,
    pub substitutions: &'a [ContractSubstitution],
}

/// A substitution accepted by a sim check. Callers should render these; an
/// accepted driver substitution must never be silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedSubstitution {
    pub component_instance: String,
    pub provider_participant_id: String,
    pub provider_artifact_id: String,
    pub provider_kind: ParticipantKind,
    pub contracts: Vec<Contract>,
}

/// A problem found while validating a robot graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// Substitutions are sim-only. Any substitution record in deploy/run mode is
    /// a hard error.
    SubstitutionNotAllowed {
        mode: PlanMode,
        component_instance: String,
        provider_participant_id: String,
    },
    /// The named substitution provider is not present in the checked graph
    /// input, so there is no participant metadata to validate against.
    SubstitutionProviderMissing {
        component_instance: String,
        provider_participant_id: String,
    },
    /// The provider exists, but is not a simulator-kind participant.
    SubstitutionProviderWrongKind {
        component_instance: String,
        provider_participant_id: String,
        provider_kind: ParticipantKind,
    },
    /// The provider is not a checked participant, so it cannot satisfy topology
    /// even if it declares matching contracts.
    SubstitutionProviderNotChecked {
        component_instance: String,
        provider_participant_id: String,
    },
    /// A simulator substitution record only covered part of a component
    /// instance's driver-side contracts.
    IncompleteSubstitution {
        component_instance: String,
        provider_participant_id: String,
        missing_contracts: Vec<Contract>,
    },
    /// Participants sharing a `(family, topic)` contract disagree on its
    /// `schema_id` (the normalized transitive wire-shape hash). Because the bus
    /// decode fast-rejects on `schema_id`, a shared contract with more than one
    /// distinct `schema_id` can never interoperate.
    ContractSchemaMismatch {
        family: String,
        topic: String,
        /// Each disagreeing `schema_id` paired with the sorted participant ids
        /// that report it. Sorted by `schema_id` so output is deterministic.
        schema_ids: Vec<(String, Vec<String>)>,
    },
    /// A query response contract is served by more than one participant.
    MultipleServerResponders {
        family: String,
        topic: String,
        /// Artifacts that serve the query response (sorted, de-duplicated).
        responders: Vec<String>,
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
    pub accepted_substitutions: Vec<AcceptedSubstitution>,
}

impl Report {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RobotGraph {
    pub component_capabilities: Vec<ComponentCapability>,
    pub motion_capabilities: BTreeSet<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ComponentCapability {
    pub instance: String,
    pub capability: String,
    pub kind: String,
}

/// Validate a robot graph: per-contract `schema_id` agreement (#16-b) + the
/// at-most-one-query-server-responder rule.
///
/// `participants` is every normal participant's `emit-apis` report. Problems are
/// returned in a stable order (schema mismatches first, by family+topic; then
/// responder conflicts by family+topic) so output and tests are deterministic.
#[must_use]
pub fn check_graph(participants: &[ParticipantApis]) -> Report {
    let robot_graph = RobotGraph::default();
    check_plan(CheckInput {
        mode: PlanMode::Deploy,
        participants,
        robot_graph: &robot_graph,
        substitutions: &[],
    })
}

#[must_use]
pub fn check_graph_with_topology(
    participants: &[ParticipantApis],
    robot_graph: &RobotGraph,
) -> Report {
    check_plan(CheckInput {
        mode: PlanMode::Deploy,
        participants,
        robot_graph,
        substitutions: &[],
    })
}

#[must_use]
pub fn check_plan(input: CheckInput<'_>) -> Report {
    let mut problems = Vec::new();
    let mut accepted_substitutions = Vec::new();
    // Expand component templates to concrete manifest topics. A template that
    // cannot expand simply contributes no concrete contracts (see module docs);
    // it is not reported as a problem.
    let MaterializedGraph { participants } =
        materialize_participants(input.participants, input.robot_graph);

    let SubstitutionValidation {
        problems: mut substitution_problems,
        mut accepted,
    } = validate_substitutions(
        input.mode,
        &participants,
        input.robot_graph,
        input.substitutions,
    );
    substitution_problems.sort_by_key(problem_sort_key);
    problems.append(&mut substitution_problems);
    accepted_substitutions.append(&mut accepted);

    // 1. Per-contract wire-shape agreement (#16-b). Group non-template contracts
    // by `(family, materialized topic)`, then by `schema_id`; a shared contract
    // used with more than one distinct `schema_id` across its participants can
    // never interoperate (the bus decode fast-rejects on `schema_id`), so it is a
    // hard mismatch. Mixed `api_version`s are allowed as long as shared contracts'
    // `schema_id`s agree. Applies to every participant, with no exemptions.
    problems.extend(schema_mismatches(&participants));

    // 2. At most one query-server responder. Producers are keyed by
    // (family, concrete topic, direction) so matching is by *kind*. Dynamic
    // component templates have already been expanded to manifest-derived concrete
    // topics; any topic still containing a placeholder contributed no contract
    // above and is skipped here too. Applies to every participant, with no
    // exemptions.
    let mut by_direction: BTreeMap<(String, String, Direction), BTreeSet<String>> = BTreeMap::new();
    for p in &participants {
        for c in &p.contracts {
            if is_template_topic(&c.topic) {
                continue;
            }
            by_direction
                .entry((c.family.clone(), c.topic.clone(), c.direction))
                .or_default()
                .insert(p.participant_id.clone());
        }
    }

    for ((family, topic, direction), producers) in &by_direction {
        if *direction == Direction::ServerResponse && producers.len() > 1 {
            problems.push(Problem::MultipleServerResponders {
                family: family.clone(),
                topic: topic.clone(),
                responders: producers.iter().cloned().collect(),
            });
        }
    }

    Report {
        problems,
        accepted_substitutions,
    }
}

struct SubstitutionValidation {
    problems: Vec<Problem>,
    accepted: Vec<AcceptedSubstitution>,
}

fn validate_substitutions(
    mode: PlanMode,
    participants: &[ParticipantApis],
    robot_graph: &RobotGraph,
    substitutions: &[ContractSubstitution],
) -> SubstitutionValidation {
    let mut problems = Vec::new();
    let mut accepted = Vec::new();

    if mode != PlanMode::Sim {
        problems.extend(
            substitutions
                .iter()
                .map(|substitution| Problem::SubstitutionNotAllowed {
                    mode,
                    component_instance: substitution.component_instance.clone(),
                    provider_participant_id: substitution.provider_participant_id.clone(),
                }),
        );
        return SubstitutionValidation { problems, accepted };
    }

    let participants_by_id = participants
        .iter()
        .map(|participant| (participant.participant_id.as_str(), participant))
        .collect::<BTreeMap<_, _>>();

    for substitution in substitutions {
        let Some(provider) = participants_by_id.get(substitution.provider_participant_id.as_str())
        else {
            problems.push(Problem::SubstitutionProviderMissing {
                component_instance: substitution.component_instance.clone(),
                provider_participant_id: substitution.provider_participant_id.clone(),
            });
            continue;
        };

        if !provider.participant_kind.is_simulator() {
            problems.push(Problem::SubstitutionProviderWrongKind {
                component_instance: substitution.component_instance.clone(),
                provider_participant_id: substitution.provider_participant_id.clone(),
                provider_kind: provider.participant_kind.clone(),
            });
            continue;
        }

        if !provider.participant_class.is_checked() {
            problems.push(Problem::SubstitutionProviderNotChecked {
                component_instance: substitution.component_instance.clone(),
                provider_participant_id: substitution.provider_participant_id.clone(),
            });
            continue;
        }

        let expected = materialize_substitution_contracts(substitution, robot_graph);
        let provider_contracts = provider.contracts.iter().cloned().collect::<BTreeSet<_>>();
        let missing_contracts = expected
            .iter()
            .filter(|contract| !provider_contracts.contains(*contract))
            .cloned()
            .collect::<Vec<_>>();

        if missing_contracts.is_empty() {
            accepted.push(AcceptedSubstitution {
                component_instance: substitution.component_instance.clone(),
                provider_participant_id: substitution.provider_participant_id.clone(),
                provider_artifact_id: provider.artifact_id.clone(),
                provider_kind: provider.participant_kind.clone(),
                contracts: expected,
            });
        } else {
            problems.push(Problem::IncompleteSubstitution {
                component_instance: substitution.component_instance.clone(),
                provider_participant_id: substitution.provider_participant_id.clone(),
                missing_contracts,
            });
        }
    }

    SubstitutionValidation { problems, accepted }
}

fn materialize_substitution_contracts(
    substitution: &ContractSubstitution,
    robot_graph: &RobotGraph,
) -> Vec<Contract> {
    let scoped_participant = ParticipantApis {
        participant_id: substitution.component_instance.clone(),
        artifact_id: substitution.component_instance.clone(),
        participant_kind: ParticipantKind::Driver,
        participant_class: ParticipantClass::Checked,
        api_version: String::new(),
        bus_abi: None,
        config_schema: None,
        scope: ParticipantScope::ComponentInstance(substitution.component_instance.clone()),
        contracts: Vec::new(),
    };
    let mut contracts = substitution
        .contracts
        .iter()
        .flat_map(|contract| {
            let materialized = materialize_contract(&scoped_participant, contract, robot_graph);
            if materialized.is_empty() {
                vec![contract.clone()]
            } else {
                materialized
            }
        })
        .collect::<Vec<_>>();
    contracts.sort();
    contracts.dedup();
    contracts
}

/// Per-contract `schema_id` agreement (#16-b).
///
/// Group non-template contracts by `(family, materialized topic)`, then by
/// `schema_id`, recording the participants reporting each id. A `(family, topic)`
/// used with more than one distinct `schema_id` across its participants can never
/// interoperate, so it is reported as a `ContractSchemaMismatch`. Contracts are
/// already materialized (component templates expanded); any topic still carrying a
/// placeholder is skipped (it was reported as unresolved upstream).
fn schema_mismatches(participants: &[ParticipantApis]) -> Vec<Problem> {
    let mut by_contract: BTreeMap<(String, String), BTreeMap<String, BTreeSet<String>>> =
        BTreeMap::new();
    for p in participants {
        for c in &p.contracts {
            if is_template_topic(&c.topic) {
                continue;
            }
            by_contract
                .entry((c.family.clone(), c.topic.clone()))
                .or_default()
                .entry(c.schema_id.clone())
                .or_default()
                .insert(p.participant_id.clone());
        }
    }

    by_contract
        .into_iter()
        .filter(|(_, schema_ids)| schema_ids.len() > 1)
        .map(
            |((family, topic), schema_ids)| Problem::ContractSchemaMismatch {
                family,
                topic,
                schema_ids: schema_ids
                    .into_iter()
                    .map(|(schema_id, participants)| {
                        (schema_id, participants.into_iter().collect())
                    })
                    .collect(),
            },
        )
        .collect()
}

/// Whether a topic still carries an unexpanded component-template placeholder.
/// Such a topic must never be matched literally against another participant.
fn is_template_topic(topic: &str) -> bool {
    topic.contains("{instance}") || topic.contains("{capability}")
}

/// A stable ordering key for problems so report output and tests are
/// deterministic regardless of the order problems were appended.
fn problem_sort_key(problem: &Problem) -> (u8, String, String) {
    match problem {
        Problem::SubstitutionNotAllowed {
            mode,
            component_instance,
            provider_participant_id,
        } => (
            0,
            component_instance.clone(),
            format!("{}\u{0}{provider_participant_id}", mode.as_str()),
        ),
        Problem::SubstitutionProviderMissing {
            component_instance,
            provider_participant_id,
        } => (
            1,
            component_instance.clone(),
            provider_participant_id.clone(),
        ),
        Problem::SubstitutionProviderWrongKind {
            component_instance,
            provider_participant_id,
            ..
        } => (
            2,
            component_instance.clone(),
            provider_participant_id.clone(),
        ),
        Problem::SubstitutionProviderNotChecked {
            component_instance,
            provider_participant_id,
        } => (
            3,
            component_instance.clone(),
            provider_participant_id.clone(),
        ),
        Problem::IncompleteSubstitution {
            component_instance,
            provider_participant_id,
            ..
        } => (
            4,
            component_instance.clone(),
            provider_participant_id.clone(),
        ),
        Problem::ContractSchemaMismatch { family, topic, .. } => (5, family.clone(), topic.clone()),
        Problem::MultipleServerResponders { family, topic, .. } => {
            (6, family.clone(), topic.clone())
        }
        Problem::InvalidConfig { runtime_id, .. } => (7, runtime_id.clone(), String::new()),
    }
}

/// The graph with every component-template contract expanded to concrete,
/// manifest-derived topics. A template that expands to zero concrete instances
/// (an optional component the robot doesn't have) simply contributes no
/// contract; see the module docs.
struct MaterializedGraph {
    participants: Vec<ParticipantApis>,
}

fn materialize_participants(
    participants: &[ParticipantApis],
    robot_graph: &RobotGraph,
) -> MaterializedGraph {
    let materialized = participants
        .iter()
        .map(|participant| {
            let contracts = participant
                .contracts
                .iter()
                .flat_map(|contract| materialize_contract(participant, contract, robot_graph))
                .collect();
            ParticipantApis {
                participant_id: participant.participant_id.clone(),
                artifact_id: participant.artifact_id.clone(),
                participant_kind: participant.participant_kind.clone(),
                participant_class: participant.participant_class,
                api_version: participant.api_version.clone(),
                bus_abi: participant.bus_abi.clone(),
                config_schema: participant.config_schema.clone(),
                scope: participant.scope.clone(),
                contracts,
            }
        })
        .collect();

    MaterializedGraph {
        participants: materialized,
    }
}

fn materialize_contract(
    participant: &ParticipantApis,
    contract: &Contract,
    robot_graph: &RobotGraph,
) -> Vec<Contract> {
    if !is_template_topic(&contract.topic) {
        return vec![contract.clone()];
    }

    let kind = component_topic_kind(&contract.topic);
    let mut candidates = robot_graph
        .component_capabilities
        .iter()
        .filter(|capability| kind.is_none_or(|kind| capability.kind == kind))
        .filter(|capability| match &participant.scope {
            ParticipantScope::Graph => {
                !is_motion_topic_kind(kind)
                    || robot_graph.motion_capabilities.is_empty()
                    || robot_graph
                        .motion_capabilities
                        .contains(&(capability.instance.clone(), capability.capability.clone()))
            }
            ParticipantScope::ComponentInstance(instance) => capability.instance == *instance,
        })
        .collect::<Vec<_>>();
    candidates.sort();

    // A template that expands to zero concrete instances (an optional component
    // the robot doesn't have) simply contributes no contract to the graph; it is
    // not reported as a problem.
    let mut materialized = candidates
        .into_iter()
        .map(|capability| Contract {
            family: contract.family.clone(),
            topic: contract
                .topic
                .replace("{instance}", &capability.instance)
                .replace("{capability}", &capability.capability),
            direction: contract.direction,
            schema_id: contract.schema_id.clone(),
        })
        .collect::<Vec<_>>();
    materialized.sort_by(|a, b| a.topic.cmp(&b.topic).then(a.direction.cmp(&b.direction)));
    materialized.dedup();
    materialized
}

fn component_topic_kind(topic: &str) -> Option<&str> {
    let mut segments = topic.split('/');
    match (segments.next(), segments.next(), segments.next()) {
        (Some("component"), Some("{instance}" | "*"), Some(kind)) if !kind.starts_with('{') => {
            Some(kind)
        }
        _ => None,
    }
}

fn is_motion_topic_kind(kind: Option<&str>) -> bool {
    matches!(kind, Some("motor" | "encoder"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(family: &str, topic: &str, direction: Direction) -> Contract {
        contract_with_schema(family, topic, direction, "deadbeef")
    }

    fn contract_with_schema(
        family: &str,
        topic: &str,
        direction: Direction,
        schema_id: &str,
    ) -> Contract {
        Contract {
            family: family.to_string(),
            topic: topic.to_string(),
            direction,
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

    fn scoped_component_participant(
        id: &str,
        instance: &str,
        contracts: Vec<Contract>,
    ) -> ParticipantApis {
        ParticipantApis {
            // A component driver shares one artifact id across instances but is a
            // distinct participant per instance, so key the graph by the instance.
            participant_id: instance.to_string(),
            artifact_id: id.to_string(),
            participant_kind: ParticipantKind::Driver,
            participant_class: ParticipantClass::Checked,
            api_version: "y2026_1".to_string(),
            bus_abi: None,
            config_schema: None,
            scope: ParticipantScope::ComponentInstance(instance.to_string()),
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

    fn simulator_participant(id: &str, contracts: Vec<Contract>) -> ParticipantApis {
        ParticipantApis {
            participant_id: id.to_string(),
            artifact_id: "webots".to_string(),
            participant_kind: ParticipantKind::Simulator,
            participant_class: ParticipantClass::Checked,
            api_version: "y2026_1".to_string(),
            bus_abi: None,
            config_schema: None,
            scope: ParticipantScope::Graph,
            contracts,
        }
    }

    fn sim_check(
        participants: &[ParticipantApis],
        robot_graph: &RobotGraph,
        substitutions: &[ContractSubstitution],
    ) -> Report {
        check_plan(CheckInput {
            mode: PlanMode::Sim,
            participants,
            robot_graph,
            substitutions,
        })
    }

    fn robot_graph() -> RobotGraph {
        RobotGraph {
            component_capabilities: vec![
                ComponentCapability {
                    instance: "left_drive".to_string(),
                    capability: "motor".to_string(),
                    kind: "motor".to_string(),
                },
                ComponentCapability {
                    instance: "right_drive".to_string(),
                    capability: "motor".to_string(),
                    kind: "motor".to_string(),
                },
                ComponentCapability {
                    instance: "left_drive".to_string(),
                    capability: "encoder".to_string(),
                    kind: "encoder".to_string(),
                },
                ComponentCapability {
                    instance: "right_drive".to_string(),
                    capability: "encoder".to_string(),
                    kind: "encoder".to_string(),
                },
            ],
            motion_capabilities: BTreeSet::from([
                ("left_drive".to_string(), "motor".to_string()),
                ("right_drive".to_string(), "motor".to_string()),
                ("left_drive".to_string(), "encoder".to_string()),
                ("right_drive".to_string(), "encoder".to_string()),
            ]),
        }
    }

    fn single_motor_graph() -> RobotGraph {
        RobotGraph {
            component_capabilities: vec![ComponentCapability {
                instance: "left_drive".to_string(),
                capability: "motor".to_string(),
                kind: "motor".to_string(),
            }],
            motion_capabilities: BTreeSet::from([("left_drive".to_string(), "motor".to_string())]),
        }
    }

    fn motor_command(direction: Direction) -> Contract {
        contract_with_schema(
            "component::MotorCommand",
            "component/{instance}/motor/{capability}/command",
            direction,
            "motor-schema",
        )
    }

    fn materialized_motor_command(instance: &str, direction: Direction) -> Contract {
        contract_with_schema(
            "component::MotorCommand",
            &format!("component/{instance}/motor/motor/command"),
            direction,
            "motor-schema",
        )
    }

    fn motor_substitution(instance: &str) -> ContractSubstitution {
        ContractSubstitution {
            component_instance: instance.to_string(),
            provider_participant_id: "simulator".to_string(),
            contracts: vec![motor_command(Direction::Subscribe)],
        }
    }

    #[test]
    fn direction_parse_round_trips_and_rejects_unknown() {
        assert_eq!(Direction::parse("publish"), Some(Direction::Publish));
        assert_eq!(Direction::parse("subscribe"), Some(Direction::Subscribe));
        assert_eq!(
            Direction::parse("server_response"),
            Some(Direction::ServerResponse)
        );
        assert_eq!(
            Direction::parse("query_request"),
            Some(Direction::QueryRequest)
        );
        assert_eq!(Direction::parse("nonsense"), None);
    }

    #[test]
    fn plan_mode_parse_round_trips_and_rejects_unknown() {
        assert_eq!(PlanMode::parse("deploy"), Some(PlanMode::Deploy));
        assert_eq!(PlanMode::parse("run"), Some(PlanMode::Run));
        assert_eq!(PlanMode::parse("sim"), Some(PlanMode::Sim));
        assert_eq!(PlanMode::parse("simulate"), None);
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
            participant(
                "mission",
                "y2026_1",
                vec![contract(
                    "drive::Target",
                    "drive/target",
                    Direction::Publish,
                )],
            ),
            participant(
                "drive",
                "y2026_1",
                vec![contract(
                    "drive::Target",
                    "drive/target",
                    Direction::Subscribe,
                )],
            ),
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
                    contract("asset::GetRequest", "asset/get", Direction::ServerRequest),
                    contract("asset::GetResponse", "asset/get", Direction::ServerResponse),
                ],
            ),
            participant(
                "client",
                "y2026_1",
                vec![
                    contract("asset::GetRequest", "asset/get", Direction::QueryRequest),
                    contract("asset::GetResponse", "asset/get", Direction::QueryResponse),
                ],
            ),
        ];
        assert!(check_graph(&graph).is_ok());
    }

    #[test]
    fn duplicate_checked_query_servers_report_multiple_server_responders() {
        let graph = vec![
            participant(
                "asset-beta",
                "y2026_1",
                vec![
                    contract("asset::GetRequest", "asset/get", Direction::ServerRequest),
                    contract("asset::GetResponse", "asset/get", Direction::ServerResponse),
                ],
            ),
            participant(
                "asset-alpha",
                "y2026_1",
                vec![
                    contract("asset::GetRequest", "asset/get", Direction::ServerRequest),
                    contract("asset::GetResponse", "asset/get", Direction::ServerResponse),
                    contract("asset::GetResponse", "asset/get", Direction::ServerResponse),
                ],
            ),
            participant(
                "client",
                "y2026_1",
                vec![
                    contract("asset::GetRequest", "asset/get", Direction::QueryRequest),
                    contract("asset::GetResponse", "asset/get", Direction::QueryResponse),
                ],
            ),
        ];

        let report = check_graph(&graph);

        assert_eq!(
            report.problems,
            vec![Problem::MultipleServerResponders {
                family: "asset::GetResponse".to_string(),
                topic: "asset/get".to_string(),
                responders: vec!["asset-alpha".to_string(), "asset-beta".to_string()],
            }]
        );
    }

    #[test]
    fn single_checked_query_server_has_no_multiple_server_responders() {
        let graph = vec![
            participant(
                "asset",
                "y2026_1",
                vec![
                    contract("asset::GetRequest", "asset/get", Direction::ServerRequest),
                    contract("asset::GetResponse", "asset/get", Direction::ServerResponse),
                ],
            ),
            participant(
                "client",
                "y2026_1",
                vec![
                    contract("asset::GetRequest", "asset/get", Direction::QueryRequest),
                    contract("asset::GetResponse", "asset/get", Direction::QueryResponse),
                ],
            ),
        ];

        let report = check_graph(&graph);

        assert!(report.is_ok());
        assert!(
            report
                .problems
                .iter()
                .all(|problem| !matches!(problem, Problem::MultipleServerResponders { .. }))
        );
    }

    #[test]
    fn contract_schema_mismatch_is_reported_per_contract() {
        // Two participants share a `(family, topic)` contract but report DIFFERENT
        // `schema_id`s -> one `ContractSchemaMismatch` naming each disagreeing id
        // and who reports it. Mixed api_versions on their own are allowed.
        let graph = vec![
            participant(
                "mission",
                "y2026_1",
                vec![contract_with_schema(
                    "drive::Target",
                    "drive/target",
                    Direction::Publish,
                    "aaaa",
                )],
            ),
            participant(
                "drive",
                "y2026_2",
                vec![contract_with_schema(
                    "drive::Target",
                    "drive/target",
                    Direction::Subscribe,
                    "bbbb",
                )],
            ),
        ];
        let report = check_graph(&graph);
        assert_eq!(
            report.problems,
            vec![Problem::ContractSchemaMismatch {
                family: "drive::Target".to_string(),
                topic: "drive/target".to_string(),
                schema_ids: vec![
                    ("aaaa".to_string(), vec!["mission".to_string()]),
                    ("bbbb".to_string(), vec!["drive".to_string()]),
                ],
            }]
        );
    }

    #[test]
    fn matching_contract_schema_ids_pass_even_with_mixed_api_versions() {
        // Same `(family, topic)` and same `schema_id` on both sides -> no problem,
        // even though the two participants report different api_versions.
        let graph = vec![
            participant(
                "mission",
                "y2026_1",
                vec![contract_with_schema(
                    "drive::Target",
                    "drive/target",
                    Direction::Publish,
                    "cafe",
                )],
            ),
            participant(
                "drive",
                "y2026_2",
                vec![contract_with_schema(
                    "drive::Target",
                    "drive/target",
                    Direction::Subscribe,
                    "cafe",
                )],
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
                vec![contract_with_schema(
                    "drive::Target",
                    "drive/target",
                    Direction::Publish,
                    "aaaa",
                )],
            ),
            participant(
                "drive",
                "y2026_1",
                vec![contract_with_schema(
                    "drive::Target",
                    "drive/target",
                    Direction::Subscribe,
                    "aaaa",
                )],
            ),
            privileged_participant(
                "inspector",
                "y2026_1",
                vec![contract_with_schema(
                    "drive::Target",
                    "drive/target",
                    Direction::Subscribe,
                    "bbbb",
                )],
            ),
        ];

        let report = check_graph(&graph);

        assert_eq!(
            report.problems,
            vec![Problem::ContractSchemaMismatch {
                family: "drive::Target".to_string(),
                topic: "drive/target".to_string(),
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
    fn component_templates_expand_to_concrete_manifest_topics() {
        let participants = vec![
            participant(
                "motion",
                "y2026_1",
                vec![contract(
                    "component::MotorCommand",
                    "component/{instance}/motor/{capability}/command",
                    Direction::Publish,
                )],
            ),
            scoped_component_participant(
                "left-driver",
                "left_drive",
                vec![contract(
                    "component::MotorCommand",
                    "component/{instance}/motor/{capability}/command",
                    Direction::Subscribe,
                )],
            ),
        ];

        let report = check_graph_with_topology(&participants, &robot_graph());

        assert!(report.problems.is_empty());
    }

    #[test]
    fn component_template_expanding_to_zero_instances_is_legal() {
        // An optional component the robot doesn't have: the template still
        // expands against the manifest, matches zero concrete instances, and
        // simply contributes no contract to the graph. No problem is reported,
        // regardless of whether the template side would otherwise be a producer
        // or a consumer.
        let participants = vec![
            participant(
                "motion",
                "y2026_1",
                vec![contract(
                    "component::MotorCommand",
                    "component/{instance}/motor/{capability}/command",
                    Direction::Publish,
                )],
            ),
            participant(
                "phantom-driver",
                "y2026_1",
                vec![contract(
                    "component::MotorCommand",
                    "component/{instance}/motor/{capability}/command",
                    Direction::Subscribe,
                )],
            ),
        ];

        // Empty robot graph: no component instances/capabilities exist.
        let report = check_graph_with_topology(&participants, &RobotGraph::default());

        assert_eq!(report, Report::default());
    }

    #[test]
    fn component_driver_template_missing_capability_is_legal() {
        // A component driver scoped to an instance that lacks the capability the
        // template needs (here: encoder, while the instance only has a motor):
        // the template contributes no contract, and the robot passes check with
        // no problems - the optional capability is simply absent.
        let graph = RobotGraph {
            component_capabilities: vec![ComponentCapability {
                instance: "left_drive".to_string(),
                capability: "motor".to_string(),
                kind: "motor".to_string(),
            }],
            motion_capabilities: BTreeSet::new(),
        };
        let participants = vec![scoped_component_participant(
            "ddsm115",
            "left_drive",
            vec![contract(
                "component::EncoderSample",
                "component/{instance}/encoder/{capability}/sample",
                Direction::Publish,
            )],
        )];

        let report = check_graph_with_topology(&participants, &graph);

        assert_eq!(report, Report::default());
    }

    #[test]
    fn sim_without_component_substitutions_reports_none() {
        let participants = vec![simulator_participant("simulator", Vec::new())];

        let report = sim_check(&participants, &RobotGraph::default(), &[]);

        assert!(report.is_ok());
        assert!(report.accepted_substitutions.is_empty());
    }

    #[test]
    fn sim_substitution_is_accepted_and_reported_for_one_instance() {
        let graph = single_motor_graph();
        let participants = vec![
            participant("motion", "y2026_1", vec![motor_command(Direction::Publish)]),
            simulator_participant("simulator", vec![motor_command(Direction::Subscribe)]),
        ];
        let substitutions = vec![motor_substitution("left_drive")];

        let report = sim_check(&participants, &graph, &substitutions);

        assert!(report.problems.is_empty());
        assert_eq!(
            report.accepted_substitutions,
            vec![AcceptedSubstitution {
                component_instance: "left_drive".to_string(),
                provider_participant_id: "simulator".to_string(),
                provider_artifact_id: "webots".to_string(),
                provider_kind: ParticipantKind::Simulator,
                contracts: vec![materialized_motor_command(
                    "left_drive",
                    Direction::Subscribe
                )],
            }]
        );
    }

    #[test]
    fn sim_substitution_still_checks_schema_agreement_with_services() {
        let graph = single_motor_graph();
        let participants = vec![
            participant(
                "motion",
                "y2026_1",
                vec![contract_with_schema(
                    "component::MotorCommand",
                    "component/{instance}/motor/{capability}/command",
                    Direction::Publish,
                    "service-schema",
                )],
            ),
            simulator_participant(
                "simulator",
                vec![contract_with_schema(
                    "component::MotorCommand",
                    "component/{instance}/motor/{capability}/command",
                    Direction::Subscribe,
                    "sim-schema",
                )],
            ),
        ];
        let substitutions = vec![ContractSubstitution {
            component_instance: "left_drive".to_string(),
            provider_participant_id: "simulator".to_string(),
            contracts: vec![contract_with_schema(
                "component::MotorCommand",
                "component/{instance}/motor/{capability}/command",
                Direction::Subscribe,
                "sim-schema",
            )],
        }];

        let report = sim_check(&participants, &graph, &substitutions);

        assert_eq!(
            report.problems,
            vec![Problem::ContractSchemaMismatch {
                family: "component::MotorCommand".to_string(),
                topic: "component/left_drive/motor/motor/command".to_string(),
                schema_ids: vec![
                    ("service-schema".to_string(), vec!["motion".to_string()]),
                    ("sim-schema".to_string(), vec!["simulator".to_string()]),
                ],
            }]
        );
        assert_eq!(report.accepted_substitutions.len(), 1);
    }

    #[test]
    fn sim_substitutions_are_scoped_per_component_instance() {
        let graph = robot_graph();
        let participants = vec![
            participant("motion", "y2026_1", vec![motor_command(Direction::Publish)]),
            simulator_participant("simulator", vec![motor_command(Direction::Subscribe)]),
        ];
        let substitutions = vec![
            motor_substitution("left_drive"),
            motor_substitution("right_drive"),
        ];

        let report = sim_check(&participants, &graph, &substitutions);

        assert!(report.problems.is_empty());
        assert_eq!(
            report
                .accepted_substitutions
                .iter()
                .map(|accepted| (
                    accepted.component_instance.as_str(),
                    accepted.contracts[0].topic.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("left_drive", "component/left_drive/motor/motor/command"),
                ("right_drive", "component/right_drive/motor/motor/command"),
            ]
        );
    }

    #[test]
    fn simulator_not_covering_an_instance_reports_missing_contracts() {
        let graph = robot_graph();
        let participants = vec![
            participant("motion", "y2026_1", vec![motor_command(Direction::Publish)]),
            simulator_participant(
                "simulator",
                vec![materialized_motor_command(
                    "left_drive",
                    Direction::Subscribe,
                )],
            ),
        ];
        let substitutions = vec![
            motor_substitution("left_drive"),
            motor_substitution("right_drive"),
        ];

        let report = sim_check(&participants, &graph, &substitutions);

        assert_eq!(
            report.problems,
            vec![Problem::IncompleteSubstitution {
                component_instance: "right_drive".to_string(),
                provider_participant_id: "simulator".to_string(),
                missing_contracts: vec![materialized_motor_command(
                    "right_drive",
                    Direction::Subscribe
                )],
            }]
        );
        assert_eq!(
            report
                .accepted_substitutions
                .iter()
                .map(|accepted| accepted.component_instance.as_str())
                .collect::<Vec<_>>(),
            vec!["left_drive"]
        );
    }

    #[test]
    fn deploy_and_run_reject_any_substitution_record() {
        let substitution = motor_substitution("left_drive");

        for mode in [PlanMode::Deploy, PlanMode::Run] {
            let report = check_plan(CheckInput {
                mode,
                participants: &[],
                robot_graph: &RobotGraph::default(),
                substitutions: std::slice::from_ref(&substitution),
            });

            assert_eq!(
                report.problems,
                vec![Problem::SubstitutionNotAllowed {
                    mode,
                    component_instance: "left_drive".to_string(),
                    provider_participant_id: "simulator".to_string(),
                }]
            );
            assert!(report.accepted_substitutions.is_empty());
        }
    }

    #[test]
    fn tool_kind_participant_cannot_provide_a_sim_substitution() {
        let graph = single_motor_graph();
        let participants = vec![
            participant("motion", "y2026_1", vec![motor_command(Direction::Publish)]),
            privileged_participant(
                "inspector",
                "y2026_1",
                vec![motor_command(Direction::Subscribe)],
            ),
        ];
        let substitutions = vec![ContractSubstitution {
            component_instance: "left_drive".to_string(),
            provider_participant_id: "inspector".to_string(),
            contracts: vec![motor_command(Direction::Subscribe)],
        }];

        let report = sim_check(&participants, &graph, &substitutions);

        assert_eq!(
            report.problems,
            vec![Problem::SubstitutionProviderWrongKind {
                component_instance: "left_drive".to_string(),
                provider_participant_id: "inspector".to_string(),
                provider_kind: ParticipantKind::Tool,
            }]
        );
        assert!(report.accepted_substitutions.is_empty());
    }

    #[test]
    fn a_publisher_anywhere_satisfies_all_subscribers() {
        let graph = vec![
            participant(
                "odometry",
                "y2026_1",
                vec![contract(
                    "odometry::State",
                    "odometry/state",
                    Direction::Publish,
                )],
            ),
            participant(
                "localize",
                "y2026_1",
                vec![contract(
                    "odometry::State",
                    "odometry/state",
                    Direction::Subscribe,
                )],
            ),
            participant(
                "map",
                "y2026_1",
                vec![contract(
                    "odometry::State",
                    "odometry/state",
                    Direction::Subscribe,
                )],
            ),
        ];
        assert!(check_graph(&graph).is_ok());
    }

    #[test]
    fn dangling_consumers_dangling_servers_and_unmatched_templates_are_all_legal() {
        // A real robot: a consumed command with no producer in the checked set
        // (an external operator/joystick/sim-controller sends it), a query server
        // with no current client (the caller is an external tool), and a
        // component-capability template that expands to zero instances (an
        // optional component - here, an e-stop button - the robot doesn't have).
        // None of these are problems or warnings; the bus tolerates all three at
        // runtime.
        let participants = vec![
            // Consumes a command nobody in the graph produces.
            participant(
                "mission",
                "y2026_1",
                vec![contract(
                    "mission::Command",
                    "mission/command",
                    Direction::Subscribe,
                )],
            ),
            // Offers a query endpoint with no client in the graph.
            participant(
                "asset",
                "y2026_1",
                vec![
                    contract("asset::GetRequest", "asset/get", Direction::ServerRequest),
                    contract("asset::GetResponse", "asset/get", Direction::ServerResponse),
                ],
            ),
            // Subscribes to an optional component capability the robot lacks.
            participant(
                "safety",
                "y2026_1",
                vec![contract(
                    "component::EmergencyStopState",
                    "component/{instance}/emergency_stop/{capability}/state",
                    Direction::Subscribe,
                )],
            ),
        ];

        // No emergency_stop capability anywhere in the robot graph.
        let robot_graph = RobotGraph {
            component_capabilities: vec![ComponentCapability {
                instance: "left_drive".to_string(),
                capability: "motor".to_string(),
                kind: "motor".to_string(),
            }],
            motion_capabilities: BTreeSet::new(),
        };

        let report = check_graph_with_topology(&participants, &robot_graph);

        assert_eq!(report, Report::default());
    }

    #[test]
    fn empty_graph_is_ok() {
        assert!(check_graph(&[]).is_ok());
    }
}
