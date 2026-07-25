//! Graph validation for Phoxal participant graphs (D59/D63/D1), plus the
//! deployment coherence pass (coherence-gate design doc, `organization`
//! `tmp/coherence-gate/readme.md`).
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
//! Nothing about pub/sub topology or query responder counts is checked by
//! `check_plan`/`check_graph`. A robot legitimately consumes commands whose
//! sender is external to the checked participant set (an operator UI, a
//! joystick, a sim controller), and legitimately offers query endpoints that
//! nothing on-robot currently calls (the callers are external tools). The bus
//! already makes these non-issues at runtime: a publisher checks for
//! subscribers before sending, and a query server starts regardless of current
//! clients. So a consumed contract with no producer and a produced contract
//! with no consumer are both legal states on their own - **that open-world
//! stance stays.**
//!
//! It has exactly one exception, and it is a cardinality rule rather than a
//! topology one: the coherence pass rejects a graph with **more than one
//! publisher of the world clock**, because two timeline authorities mean two
//! world histories advancing the same participants (#952 section C). Zero is
//! still legal - a real robot has no world authority at all.
//!
//! What the open-world stance does not cover is a contract that is
//! demonstrably produced/served in-set at one version while a participant
//! consumes/asks a *different, disjoint* version of that same logical
//! contract - clear evidence of a botched migration, not an external
//! counterpart. [`check_coherence`] is that one contract-axis check: a pure,
//! mismatch-only pass over each participant's `#[derive(phoxal::Api)]`
//! contract surface (the `version`/`contract`/`role`/`external` entries of
//! [`crate::participant::metadata::ParticipantMetaContract`]). It blocks only
//! on version disjointness where a counterpart is provably in-set, never on
//! absence alone - see its own docs and the coherence-gate design doc §3 for
//! the exact rule and worked examples.
//!
//! Simulation plans differ from deploy/run plans only in *which participants the
//! caller passes*: in sim, a component driver is simply not launched and the
//! Webots simulator participant is passed in its place (D16). Because the
//! simulator is built from the same framework, it speaks the same contracts by
//! construction. There is no separate substitution concept, no completeness
//! gate, and no missing-producer diagnostic here - whether a contract has a
//! producer is a caller/deployment choice, not something this checker judges.

use std::collections::{BTreeMap, BTreeSet};

use crate::participant::metadata::ParticipantMetaContract;

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

// ---------------------------------------------------------------------------
// Coherence pass (coherence-gate design doc §3)
// ---------------------------------------------------------------------------

/// One participant's contract surface for [`check_coherence`]: its identity
/// plus every `{role, version, contract, external}` entry recorded by its
/// `#[derive(phoxal::Api)]` metadata section (design doc §2).
///
/// Deliberately separate from [`ParticipantApis`] above: that type identifies
/// a contract by a single version-qualified `family` string with no role
/// axis (it feeds the topology/config checker, `check_plan`), while the
/// coherence pass needs `version` and `contract` as separate fields plus
/// the `role`/`external` facts to group by logical contract and apply the
/// role-asymmetric rule (design doc §2 "the check never parses a name").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantContractSurface {
    /// The participant identity named in diagnostics.
    pub participant_id: String,
    /// This participant's contract surface entries, in any order.
    pub contracts: Vec<ParticipantMetaContract>,
}

/// One coherence mismatch found by [`check_coherence`] (design doc §3).
///
/// Both variants block only on **positive disjointness evidence** - a
/// counterpart that is demonstrably present in-set at versions that share
/// nothing with this participant's own. Absence alone (an empty `Pub(L)` or
/// `Serve(L)`) is never a mismatch; that is exactly the open-world stance the
/// module docs describe, which this pass narrows only where evidence exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoherenceMismatch {
    /// A participant's non-`external` `subscribe` versions for a logical
    /// contract share no version with any in-set publisher of that
    /// contract, even though the contract *is* published in-set (design doc
    /// §3, "pub/sub - per-participant overlap"). Checked per subscribing
    /// participant, never pooled: one up-to-date subscriber elsewhere does
    /// not excuse another participant's stranded hard cutover.
    PubSubDisjoint {
        participant_id: String,
        /// The logical contract (the `contract` field alone; version-independent).
        contract: String,
        /// This participant's non-`external` `subscribe` versions for `contract`.
        subscribed: BTreeSet<String>,
        /// Every version published in-set for `contract`.
        published: BTreeSet<String>,
    },
    /// A participant's non-`external` `ask` version for a logical contract
    /// has no same-version `serve` in-set: a permanently dead query
    /// (design doc §3, "serve/ask - every ask matched"; `QueryError::Timeout`,
    /// D31). The one closed-world rule in the design - checked per ask
    /// version, since a server superset elsewhere in the same contract
    /// does not answer a different, unserved version.
    UnservedAsk {
        participant_id: String,
        /// The logical contract (the `contract` field alone; version-independent).
        contract: String,
        /// The ask version with no matching in-set server.
        version: String,
        /// Every version served in-set for `contract` (may be empty).
        served: BTreeSet<String>,
    },
    /// More than one participant publishes the world clock, so the graph has
    /// more than one timeline authority (#952 section C).
    ///
    /// This is the single closed-world exception on the publish side, and it is
    /// a cardinality rule rather than a version rule: the world clock is not a
    /// contract several producers can each contribute a view of. Two authorities
    /// mean two world histories advancing the same participants, which no
    /// consumer can reconcile - a subscriber cannot tell which one it is
    /// stepping on. Zero publishers stays legal, because a real robot has no
    /// world authority at all.
    MultipleTimelineAuthorities {
        /// Every participant publishing the world clock, in set order.
        participant_ids: BTreeSet<String>,
    },
}

/// The outcome of the coherence pass: the mismatches found (empty == coherent).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CoherenceReport {
    pub mismatches: Vec<CoherenceMismatch>,
}

impl CoherenceReport {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// Run the deployment coherence pass over a whole participant set (design doc
/// §3).
///
/// Pure and total: no I/O, no building, no binary parsing - callers hand it
/// already-parsed contract surfaces. Groups every entry by its **logical
/// contract** (the `contract` field alone, version-independent) and
/// applies the role-asymmetric rule:
///
/// - **Pub/sub** blocks a subscribing participant only when the contract *is*
///   published in-set (`Pub(L)` non-empty) and this participant's own
///   non-`external` subscribe versions share nothing with it. Checked
///   per-participant, not pooled, so one dual-subscribing participant never
///   masks another's stranded hard cutover.
/// - **Serve/ask** blocks every non-`external` ask version that has no
///   same-version server in-set, regardless of whether the contract is
///   served at *other* versions - the one closed-world rule in the design,
///   because an unmatched ask never resolves to a useful intermediate state
///   (it times out on every call, D31), unlike an unmatched subscribe.
///
/// An `external` entry (`#[phoxal(external)]`, design doc §1) skips its own
/// edge's requirement entirely: it is excluded from `SubP(L)`/`AskP(L)` before
/// either check runs, so a participant whose only edge for a contract is
/// marked `external` is never flagged for it.
/// The contract whose publisher *is* the timeline authority.
///
/// Named here rather than derived from the api tree because the rule is about
/// this one contract's meaning, not about any structural property a checker
/// could infer: publishing the world clock is what makes a participant the
/// authority over world history (#952 section C).
pub const TIMELINE_AUTHORITY_CONTRACT: &str = "simulation::Clock";

#[must_use]
pub fn check_coherence(participants: &[ParticipantContractSurface]) -> CoherenceReport {
    // Pub(L)/Serve(L), pooled across the whole set (never filtered by
    // `external`: the marker only ever applies to consumer roles, design doc
    // §1, so every publish/serve entry counts toward these pools).
    let mut published: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut served: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for p in participants {
        for c in &p.contracts {
            match c.role.as_str() {
                "publish" => {
                    published
                        .entry(c.contract.as_str())
                        .or_default()
                        .insert(c.version.as_str());
                }
                "serve" => {
                    served
                        .entry(c.contract.as_str())
                        .or_default()
                        .insert(c.version.as_str());
                }
                _ => {}
            }
        }
    }

    let mut mismatches = Vec::new();

    // The one publisher-cardinality rule (see `MultipleTimelineAuthorities`).
    let authorities: BTreeSet<String> = participants
        .iter()
        .filter(|p| {
            p.contracts
                .iter()
                .any(|c| c.role == "publish" && c.contract == TIMELINE_AUTHORITY_CONTRACT)
        })
        .map(|p| p.participant_id.clone())
        .collect();
    if authorities.len() > 1 {
        mismatches.push(CoherenceMismatch::MultipleTimelineAuthorities {
            participant_ids: authorities,
        });
    }

    for p in participants {
        // Pub/sub: per-participant SubP(L), non-external edges only.
        let mut sub_by_contract: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for c in &p.contracts {
            if c.role == "subscribe" && !c.external {
                sub_by_contract
                    .entry(c.contract.as_str())
                    .or_default()
                    .insert(c.version.as_str());
            }
        }
        for (contract, subscribed) in sub_by_contract {
            let Some(published) = published.get(contract) else {
                continue; // Pub(L) empty -> open world, OK.
            };
            if subscribed.is_disjoint(published) {
                mismatches.push(CoherenceMismatch::PubSubDisjoint {
                    participant_id: p.participant_id.clone(),
                    contract: contract.to_string(),
                    subscribed: subscribed.iter().map(|s| (*s).to_string()).collect(),
                    published: published.iter().map(|s| (*s).to_string()).collect(),
                });
            }
        }

        // Serve/ask: every non-external ask version needs a same-version server.
        for c in &p.contracts {
            if c.role != "ask" || c.external {
                continue;
            }
            let contract_served = served.get(c.contract.as_str());
            let has_server = contract_served.is_some_and(|gens| gens.contains(c.version.as_str()));
            if !has_server {
                mismatches.push(CoherenceMismatch::UnservedAsk {
                    participant_id: p.participant_id.clone(),
                    contract: c.contract.clone(),
                    version: c.version.clone(),
                    served: contract_served
                        .map(|gens| gens.iter().map(|s| (*s).to_string()).collect())
                        .unwrap_or_default(),
                });
            }
        }
    }

    CoherenceReport { mismatches }
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
    // check_coherence (coherence-gate design doc §3 worked examples)
    // -----------------------------------------------------------------

    fn meta_contract(
        role: &str,
        version: &str,
        contract: &str,
        external: bool,
    ) -> ParticipantMetaContract {
        ParticipantMetaContract {
            role: role.to_string(),
            version: version.to_string(),
            contract: contract.to_string(),
            external,
        }
    }

    fn surface(id: &str, contracts: Vec<ParticipantMetaContract>) -> ParticipantContractSurface {
        ParticipantContractSurface {
            participant_id: id.to_string(),
            contracts,
        }
    }

    fn gens(vals: &[&str]) -> BTreeSet<String> {
        vals.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn coherence_empty_participant_set_is_ok() {
        assert!(check_coherence(&[]).is_ok());
    }

    /// #952 section C: the graph admits exactly one timeline authority. Zero is
    /// the ordinary case (a real robot), one is a simulation, and two is a
    /// graph whose participants would be stepped by two different world
    /// histories at once.
    #[test]
    fn coherence_rejects_a_second_timeline_authority_but_allows_none() {
        let clock = || {
            vec![meta_contract(
                "publish",
                "v0.1",
                TIMELINE_AUTHORITY_CONTRACT,
                false,
            )]
        };
        let subscriber = surface(
            "drive",
            vec![meta_contract(
                "subscribe",
                "v0.1",
                TIMELINE_AUTHORITY_CONTRACT,
                false,
            )],
        );

        assert!(
            check_coherence(std::slice::from_ref(&subscriber)).is_ok(),
            "a real robot has no world authority at all"
        );
        assert!(
            check_coherence(&[surface("webots", clock()), subscriber.clone()]).is_ok(),
            "exactly one authority is the simulation case"
        );

        let report = check_coherence(&[
            surface("webots", clock()),
            surface("second-controller", clock()),
            subscriber,
        ]);
        assert_eq!(
            report.mismatches,
            vec![CoherenceMismatch::MultipleTimelineAuthorities {
                participant_ids: gens(&["second-controller", "webots"]),
            }]
        );
    }

    /// §3 worked example 1: "pub `v1`, sub `v2`, nothing else ->
    /// disjoint -> block".
    #[test]
    fn coherence_pub_sub_disjoint_versions_block() {
        let participants = vec![
            surface(
                "drive",
                vec![meta_contract("publish", "v1", "drive::Target", false)],
            ),
            surface(
                "mission",
                vec![meta_contract("subscribe", "v2", "drive::Target", false)],
            ),
        ];

        let report = check_coherence(&participants);

        assert_eq!(
            report.mismatches,
            vec![CoherenceMismatch::PubSubDisjoint {
                participant_id: "mission".to_string(),
                contract: "drive::Target".to_string(),
                subscribed: gens(&["v2"]),
                published: gens(&["v1"]),
            }]
        );
    }

    /// §3 worked example 2: "pub `v1`; one service subscribes both
    /// `v1` and `v2` -> valid" (the dual-subscribe spans a
    /// migration).
    #[test]
    fn coherence_dual_subscribe_spans_migration_is_valid() {
        let participants = vec![
            surface(
                "drive",
                vec![meta_contract("publish", "v1", "drive::Target", false)],
            ),
            surface(
                "mission",
                vec![
                    meta_contract("subscribe", "v1", "drive::Target", false),
                    meta_contract("subscribe", "v2", "drive::Target", false),
                ],
            ),
        ];

        assert!(check_coherence(&participants).is_ok());
    }

    /// §3 worked example 3: the same service then drops its `v1` sub ->
    /// `SubP={7}` vs `Pub={1}` -> disjoint -> block.
    #[test]
    fn coherence_dropping_old_sub_after_dual_subscribe_blocks() {
        let participants = vec![
            surface(
                "drive",
                vec![meta_contract("publish", "v1", "drive::Target", false)],
            ),
            surface(
                "mission",
                vec![meta_contract("subscribe", "v2", "drive::Target", false)],
            ),
        ];

        let report = check_coherence(&participants);

        assert_eq!(
            report.mismatches,
            vec![CoherenceMismatch::PubSubDisjoint {
                participant_id: "mission".to_string(),
                contract: "drive::Target".to_string(),
                subscribed: gens(&["v2"]),
                published: gens(&["v1"]),
            }]
        );
    }

    /// §3 worked example 4 (the pooled-rule trap): "pub `v1`; service A
    /// subs `{1}`; service B subs `{7}` only -> A passes, B blocks". A pooled
    /// `Pub(L) ∩ Sub(L)` over the union would wrongly pass this via A's
    /// overlap; the per-participant rule must flag B specifically.
    #[test]
    fn coherence_pooled_overlap_does_not_excuse_a_stranded_subscriber() {
        let participants = vec![
            surface(
                "drive",
                vec![meta_contract("publish", "v1", "drive::Target", false)],
            ),
            surface(
                "service_a",
                vec![meta_contract("subscribe", "v1", "drive::Target", false)],
            ),
            surface(
                "service_b",
                vec![meta_contract("subscribe", "v2", "drive::Target", false)],
            ),
        ];

        let report = check_coherence(&participants);

        assert_eq!(
            report.mismatches,
            vec![CoherenceMismatch::PubSubDisjoint {
                participant_id: "service_b".to_string(),
                contract: "drive::Target".to_string(),
                subscribed: gens(&["v2"]),
                published: gens(&["v1"]),
            }]
        );
    }

    /// §3 ask worked example 1: "servers `{L, L+1}`, ask `{L+1}` -> valid"
    /// (the extra server@L is a harmless unused server).
    #[test]
    fn coherence_ask_matched_by_a_superset_server_is_valid() {
        let participants = vec![
            surface(
                "asset",
                vec![
                    meta_contract("serve", "v1", "asset::Get", false),
                    meta_contract("serve", "v2", "asset::Get", false),
                ],
            ),
            surface(
                "client",
                vec![meta_contract("ask", "v2", "asset::Get", false)],
            ),
        ];

        assert!(check_coherence(&participants).is_ok());
    }

    /// §3 ask worked example 2: "server `{L}`, ask `{L+1}` -> block".
    #[test]
    fn coherence_ask_with_no_matching_server_version_blocks() {
        let participants = vec![
            surface(
                "asset",
                vec![meta_contract("serve", "v1", "asset::Get", false)],
            ),
            surface(
                "client",
                vec![meta_contract("ask", "v2", "asset::Get", false)],
            ),
        ];

        let report = check_coherence(&participants);

        assert_eq!(
            report.mismatches,
            vec![CoherenceMismatch::UnservedAsk {
                participant_id: "client".to_string(),
                contract: "asset::Get".to_string(),
                version: "v2".to_string(),
                served: gens(&["v1"]),
            }]
        );
    }

    /// §3 ask worked example 3: "server `{L}`, ask `{L, L+1}` -> ask@L is
    /// answered, but ask@L+1 has no server -> block" - only the L+1 ask.
    #[test]
    fn coherence_partial_ask_coverage_blocks_only_the_unserved_version() {
        let participants = vec![
            surface(
                "asset",
                vec![meta_contract("serve", "v1", "asset::Get", false)],
            ),
            surface(
                "client",
                vec![
                    meta_contract("ask", "v1", "asset::Get", false),
                    meta_contract("ask", "v2", "asset::Get", false),
                ],
            ),
        ];

        let report = check_coherence(&participants);

        assert_eq!(
            report.mismatches,
            vec![CoherenceMismatch::UnservedAsk {
                participant_id: "client".to_string(),
                contract: "asset::Get".to_string(),
                version: "v2".to_string(),
                served: gens(&["v1"]),
            }]
        );
    }

    /// §1: `#[phoxal(external)]` on a subscribe edge skips that edge's
    /// pub/sub requirement entirely, even though the versions are
    /// otherwise disjoint from the in-set publisher.
    #[test]
    fn coherence_external_marker_excuses_a_subscribe_mismatch() {
        let participants = vec![
            surface(
                "drive",
                vec![meta_contract("publish", "v1", "drive::Target", false)],
            ),
            surface(
                "teleop",
                vec![meta_contract("subscribe", "v2", "drive::Target", true)],
            ),
        ];

        assert!(check_coherence(&participants).is_ok());
    }

    /// §1: `#[phoxal(external)]` on an ask edge skips that edge's
    /// serve/ask requirement entirely, even though no in-set server answers
    /// that version.
    #[test]
    fn coherence_external_marker_excuses_an_ask_mismatch() {
        let participants = vec![
            surface(
                "asset",
                vec![meta_contract("serve", "v1", "asset::Get", false)],
            ),
            surface(
                "client",
                vec![meta_contract("ask", "v2", "asset::Get", true)],
            ),
        ];

        assert!(check_coherence(&participants).is_ok());
    }

    /// §3: "if `Pub(L)` is empty -> OK (open world: the producer may be
    /// external to the set... or intentionally absent)".
    #[test]
    fn coherence_open_world_ok_when_nothing_publishes_in_set() {
        let participants = vec![surface(
            "mission",
            vec![meta_contract(
                "subscribe",
                "v1",
                "navigation::Request",
                false,
            )],
        )];

        assert!(check_coherence(&participants).is_ok());
    }

    /// §3: "a server with no matching ask is harmless (unused server,
    /// callers may be external tools)".
    #[test]
    fn coherence_server_with_no_asker_is_harmless() {
        let participants = vec![surface(
            "asset",
            vec![meta_contract("serve", "v1", "asset::Get", false)],
        )];

        assert!(check_coherence(&participants).is_ok());
    }
}
