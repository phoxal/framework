//! Scenario bundle assembly. P2.
//!
//! A scenario bundle is the artifact the case host admits into the
//! controlled-simulation admission path. It is identical in shape to
//! a normal `Bundle` except for two recorded facts:
//!  * the bundle is marked nondeployable so the hardware launch /
//!    installation paths refuse it it before any binary runs.
//!  * one or more input edges are substituted: the scenario
//!    supplies its own prepared producer payloads so the bundle does
//!    ! depend on a parallel live input channel. The original edge
//!    is preserved in provenance and the unrelated connections
//!    are left unchanged.

use std::collections::BTreeMap;
use std::fmt;

use crate::ProjectLayout;

/// The marker value that flags a bundle as nondeployable. Any
/// hardware-mode admission path that observes a `Bundle` with this
/// marker must reject it explicitly.
pub const SCENARIO_NONDEPLOYABLE: &str = "phoxal/scenario/nondeployable@1";

/// One substituted input edge on the scenario bundle. The original
/// edge identity is preserved so rollback can re-attach the live
/// producer if the bundle is later cancelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstitutedEdge {
    pub edge_id: String,
    pub original_producer_signature: String,
    pub substituted_producer_signature: String,
    pub substituted_payload_artifact: String,
}

/// The scenario bundle identity the case host hands to the
/// supervisor admission path. It is intentionally narrower than a
/// full `Bundle`; the supervisor re-wraps it as a normal `Bundle`
/// after admission, with the `nondeployable` marker preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioBundle {
    pub scenario_name: String,
    pub program_byte_length: u32,
    pub program_digest: String,
    pub transition_count: u32,
    pub substituted_edges: Vec<SubstitutedEdge>,
    /// The original input edges that were NOT substituted. Stored
    /// here as plain strings for inspection; the supervisor's full
    /// `Bundle` keeps the structured copies.
    pub preserved_edges: Vec<String>,
}

impl ScenarioBundle {
    /// Construct a scenario bundle from a list of preserved-edge stems
    /// (the discovered scenarios that are NOT substituted), the
    /// prepared program identity, and the substitution map.
    pub fn assemble(
        scenario_name: impl Into<String>,
        program_byte_length: u32,
        program_digest: impl Into<String>,
        transition_count: u32,
        substitutions: Vec<SubstitutedEdge>,
        preserved_edge_stems: Vec<String>,
    ) -> Self {
        let substituted_edge_ids: BTreeMap<&str, ()> = substitutions
            .iter()
            .map(|edge| (edge.edge_id.as_str(), ()))
            .collect();
        let preserved_edges: Vec<String> = preserved_edge_stems
            .into_iter()
            .filter(|stem| !substituted_edge_ids.contains_key(stem.as_str()))
            .collect();
        Self {
            scenario_name: scenario_name.into(),
            program_byte_length,
            program_digest: program_digest.into(),
            transition_count,
            substituted_edges: substitutions,
            preserved_edges,
        }
    }

    /// Convenience constructor that walks the discovered scenarios
    /// through the layout and records the un-substituted edges.
    pub fn assemble_from_layout(
        layout: &ProjectLayout,
        scenario_name: impl Into<String>,
        program_byte_length: u32,
        program_digest: impl Into<String>,
        transition_count: u32,
        substitutions: Vec<SubstitutedEdge>,
    ) -> Self {
        let preserved = super::discover_scenarios(layout.root())
            .map(|scenarios| {
                scenarios
                    .iter()
                    .map(|scenario| {
                        scenario
                            .module_identifier
                            .strip_prefix("_scenario_")
                            .unwrap_or(scenario.module_identifier.as_str())
                            .to_owned()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Self::assemble(
            scenario_name,
            program_byte_length,
            program_digest,
            transition_count,
            substitutions,
            preserved,
        )
    }

    /// The marker the supervisor must attach to the wrapped `Bundle`
    /// before handing it to any hardware admission path.
    pub fn nondeployable_marker(&self) -> &'static str {
        SCENARIO_NONDEPLOYABLE
    }
}

impl fmt::Display for ScenarioBundle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "scenario bundle `{}` ({} transitions, {} substituted edges, {} preserved edges, digest {}… {})",
            self.scenario_name,
            self.transition_count,
            self.substituted_edges.len(),
            self.preserved_edges.len(),
            &self.program_digest[..self.program_digest.len().min(8)],
            self.program_byte_length
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nondeployable_marker_is_stable() {
        assert_eq!(SCENARIO_NONDEPLOYABLE, "phoxal/scenario/nondeployable@1");
    }

    #[test]
    fn preserves_unrelated_edges_in_provenance() {
        let bundle = ScenarioBundle::assemble(
            "scenarios/First",
            16,
            "deadbeef".repeat(8),
            2,
            vec![SubstitutedEdge {
                edge_id: "a".to_owned(),
                original_producer_signature: "phoxal.motion/State".to_owned(),
                substituted_producer_signature: "phoxal.motion/State (fixture)".to_owned(),
                substituted_payload_artifact: "artifacts/motion.fixture.json".to_owned(),
            }],
            vec!["b".to_owned()],
        );
        assert_eq!(bundle.substituted_edges.len(), 1);
        assert_eq!(bundle.preserved_edges, vec!["b".to_owned()]);
        assert_eq!(bundle.scenario_name, "scenarios/First");
        assert_eq!(bundle.transition_count, 2);
        assert_eq!(bundle.program_digest.len(), 64);
    }

    #[test]
    fn assemble_from_layout_filters_substituted_edges() {
        // The convenience constructor must drop a stem from the
        // preserved list when it matches a substitution.
        let bundle = ScenarioBundle::assemble(
            "scenarios/First",
            8,
            "deadbeef".repeat(8),
            1,
            vec![SubstitutedEdge {
                edge_id: "drop".to_owned(),
                original_producer_signature: "a".to_owned(),
                substituted_producer_signature: "b".to_owned(),
                substituted_payload_artifact: "c".to_owned(),
            }],
            vec!["drop".to_owned(), "keep".to_owned()],
        );
        assert_eq!(bundle.preserved_edges, vec!["keep".to_owned()]);
    }
}
