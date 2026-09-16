//! Case-host protocol (P1 #3).
//!
//! The case host retains a scenario through planning, execution, and
//! verification. The macro registers a per-type entry that returns
//! the validated [`PlannedScenario`]; this module drives the rest of
//! the lifecycle by converting the plan into a typed
//! [`crate::scenario::Program`], dispatching the supervisor's
//! required-child fixture against the controlled simulation,
//! collecting typed evidence, recording terminal evidence, sealing
//! the run, and invoking the retained verifier.
//!
//! Until the supervisor / simulator provisioning lands for the SDK
//! case host, [`run_harness`] refuses with an explicit diagnostic
//! naming the missing boundary. The descriptor, plan, and macro
//! path continue to work; the lifecycle boundary is the only thing
//! left to wire up.

use thiserror::Error;

/// Failure modes the case host can return.
#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("scenario `{0}` is not registered")]
    UnknownScenario(String),
    #[error("{0}")]
    DuplicateIdentity(crate::scenario::registry::DuplicateScenarioError),
    #[error("scenario `{0}` failed: {1}")]
    ScenarioFailed(String, String),
    #[error("internal harness error: {0}")]
    Internal(String),
}

/// Aggregated result of one harness invocation. `report_artifact_path`
/// is populated once the case host materializes a sealed run artifact;
/// it remains `None` until that boundary lands.
#[derive(Debug, Clone)]
pub struct HarnessRun {
    pub name: String,
    pub passed: bool,
    pub detail: Option<String>,
    pub report_artifact_path: Option<String>,
}

/// Drive one registered scenario through planning, execution, and
/// verification.
///
/// 1. Resolve the descriptor by short name.
/// 2. Call the descriptor's entry to get a [`PlannedScenario`] (the
///    user's `plan()` returning a [`crate::scenario::ScenarioPlan`]).
/// 3. Convert the plan into a typed
///    [`crate::scenario::Program`] so the lifecycle can hand it to
///    the supervisor, fixture, and collector.
///
/// Steps 4-7 (supervisor launch, fixture dispatch, evidence
/// collection, terminal-evidence recording, verifier invocation) all
/// live in the SDK case host. Until they land, the lifecycle returns
/// `passed: false` with a diagnostic naming the missing boundary so
/// the inventory, listing, and `scenario run` commands continue to
/// report an honest result.
///
/// Returns `crate::Result` (anyhow) so the generated
/// `phoxal-scenarios` test target can `?`-propagate directly without
/// an `Into` impl. See Gate P1 #3 of followup-5d11cfc1.md.
pub fn run_harness(short_name: &str) -> crate::Result<HarnessRun> {
    let entries = crate::scenario::registry::list_scenarios().map_err(|e| crate::anyhow!("{e}"))?;

    let entry = entries
        .iter()
        .find(|e| e.short_name == short_name)
        .ok_or_else(|| {
            crate::anyhow!("{}", HarnessError::UnknownScenario(short_name.to_owned()))
        })?;

    // (1) Drive the macro-generated entry. The entry constructs the
    //     concrete scenario via `Default::default()`, calls the
    //     user's `plan()`, and hands the validated plan back to
    //     the case host.
    let planned = (entry.entry)()
        .map_err(|e| crate::anyhow!("{}", HarnessError::Internal(format!("{e:#}"))))?;

    // (2) Convert the authored plan to a typed program. The `plan()`
    //     output is the ScenarioPlan (typed setpoints, commands,
    //     captures); the case host encodes it into a Program so the
    //     supervisor and fixture consume the same wire format the
    //     publisher emits.
    //
    //     TODO(case-host): implement the supervisor + fixture
    //     lifecycle that:
    //       a. serializes the plan to a Program,
    //       b. spawns the supervisor with the controlled simulation
    //          surface and a fixture child,
    //       c. dispatches typed actions from authoritative boundaries
    //          (setpoints at boundary N → fixture input port; commands
    //          issued at boundary N → fixture output port reply),
    //       d. records step outcomes, captures, and command replies
    //          through the EvidenceCollector as the fixture emits
    //          them,
    //       e. after the final native transition, records
    //          TerminalEvidence { execution_identity, quantum_ns,
    //          completed_transitions: program.transition_count(),
    //          final_observation_cut: true, final_capture_drain:
    //          true, cleanup_ok: true },
    //       f. seals the collector to obtain a ScenarioRun,
    //       g. invokes the user's `verify()` against the ScenarioRun,
    //       h. returns the resulting pass/fail outcome.
    let run = HarnessRun {
        name: planned.name,
        passed: false,
        detail: Some(format!(
            "scenario case-host lifecycle is not yet implemented; \
             `{}` planned successfully but no supervisor + fixture \
             child + collector drove the typed execution. See Gate P1 #3 \
             of followup-5d11cfc1.md.",
            short_name
        )),
        report_artifact_path: None,
    };
    if !run.passed {
        return Err(crate::anyhow!(
            "{}",
            HarnessError::ScenarioFailed(run.name.clone(), run.detail.clone().unwrap_or_default(),)
        ));
    }
    Ok(run)
}

#[cfg(test)]
mod tests {
    use crate::scenario::ScenarioPlan;
    use crate::scenario::registry::ScenarioDescriptor;

    /// Panic in `Default::default` so any eager construction at
    /// listing or registration time would surface here. See Gate A1
    /// of followup-24c026ed.md: "Listing invokes neither Default,
    /// plan, nor verify."
    #[derive(Debug)]
    #[allow(dead_code)]
    struct PanicOnDefault;
    impl Default for PanicOnDefault {
        fn default() -> Self {
            panic!("listing must not construct the scenario");
        }
    }
    impl crate::scenario::Scenario for PanicOnDefault {
        fn plan(&self) -> crate::Result<ScenarioPlan> {
            panic!("listing must not call plan()");
        }
        fn verify(&self, _run: &crate::scenario::ScenarioRun) -> crate::Result<()> {
            panic!("listing must not call verify()");
        }
    }

    /// The descriptor's `entry` returns the validated [`PlannedScenario`]
    /// produced by the user's `plan()`. The case host drives the rest
    /// of the lifecycle; the entry itself never produces a passing
    /// outcome. Returning `ScenarioOutcome { passed: true, .. }` from
    /// the entry would be a false success path: see followup-24c026ed
    /// Gate A1 clause 3.
    #[test]
    fn entry_returns_planned_scenario_for_lifecycle_to_drive() {
        fn entry() -> crate::Result<crate::scenario::PlannedScenario> {
            // The case host is the only authority on pass/fail. The
            // entry just hands back the plan; the entry's return type
            // is `PlannedScenario`, not `ScenarioOutcome`.
            Ok(crate::scenario::PlannedScenario {
                name: "scenarios/NonPassCheckpoint".to_owned(),
                plan: crate::scenario::ScenarioPlan::new(
                    "scene",
                    std::time::Duration::from_micros(2_000),
                ),
            })
        }
        let descriptor = ScenarioDescriptor {
            name: "scenarios/NonPassCheckpoint",
            short_name: "NonPassCheckpoint",
            module_path: "test",
            source_file: "test.rs",
            source_line: 0,
            entry,
        };
        // `Default::default` would panic if listing ran it; instead
        // we just call the descriptor's `entry` directly so the
        // entry's contract (returns PlannedScenario, not an outcome)
        // is exercised.
        let planned = (descriptor.entry)().expect("entry ok");
        assert_eq!(planned.name, "scenarios/NonPassCheckpoint");
    }

    /// The registry's `list_scenarios` walks inventory but does not
    /// construct registered scenarios. The reproduction in
    /// /tmp/phoxal-gate-a/review_a1 confirms the macro expansion does
    /// not eagerly construct; this test asserts the in-tree registry
    /// helper behaves the same way: walking it never invokes a
    /// `Default` impl.
    #[test]
    fn listing_does_not_construct_registered_scenarios() {
        let entries = crate::scenario::registry::list_scenarios().expect("list scenarios");
        for entry in entries {
            // Touching only the static metadata — never call the
            // entry function. A panic-from-Default test would surface
            // here if the listing path were eager.
            let _ = (entry.name, entry.short_name);
        }
    }

    /// A scenario registered via the real `#[phoxal::scenario]`
    /// attribute with a `Default::default` impl that panics would
    /// surface any eager construction at listing or registration
    /// time. The inventory snapshot is already sorted by short name;
    /// we look up the descriptor by exact match so the test cannot
    /// silently accept another module's entry. See Gate P1 #6 of
    /// followup-5d11cfc1.md.
    #[allow(dead_code)]
    #[derive(Debug)]
    struct PanicOnDefaultScenario;

    impl Default for PanicOnDefaultScenario {
        fn default() -> Self {
            panic!("listing must not construct the scenario");
        }
    }

    #[phoxal::scenario]
    impl crate::scenario::Scenario for PanicOnDefaultScenario {
        fn plan(&self) -> crate::Result<ScenarioPlan> {
            panic!("listing must not call plan()");
        }
        fn verify(&self, _run: &crate::scenario::ScenarioRun) -> crate::Result<()> {
            panic!("listing must not call verify()");
        }
    }

    /// Listing the registry must not invoke any registered scenario's
    /// `Default::default`. The descriptor for `PanicOnDefaultScenario`
    /// is registered by the macro at compile time; a real eager
    /// construction would panic here.
    #[test]
    fn listing_does_not_invoke_registered_default_impl() {
        // Touching the static metadata only — never call the entry.
        let entries = crate::scenario::registry::list_scenarios().expect("list scenarios");
        let registered = entries
            .iter()
            .find(|e| e.short_name == "PanicOnDefaultScenario")
            .expect("macro must register PanicOnDefaultScenario");
        assert_eq!(registered.name, "scenarios/PanicOnDefaultScenario");
    }

    /// The descriptor's `entry` function (the per-type monomorphized
    /// function pointer) must construct the registered type via
    /// `Default::default()` and call `plan()`. The
    /// `PanicOnDefaultScenario` fixture's `Default` impl panics with
    /// a deterministic diagnostic so any future regression that
    /// bypasses the entry path (e.g. eager construction at listing
    /// time) surfaces here as a panic instead of silently passing.
    /// See Gate P1 #6 of followup-5d11cfc1.md.
    #[test]
    fn real_compiled_entry_constructs_via_default_then_plans() {
        let entries = crate::scenario::registry::list_scenarios().expect("list scenarios");
        let registered = entries
            .iter()
            .find(|e| e.short_name == "PanicOnDefaultScenario")
            .expect("registered by macro");
        // The macro-generated entry must call `Default::default()`
        // first; that is the documented contract. We catch the
        // fixture's deliberate panic so the test reports a useful
        // message instead of failing the process.
        let result = std::panic::catch_unwind(|| (registered.entry)());
        assert!(
            result.is_err(),
            "the macro-generated entry should call Default::default() and \
             then plan(); the PanicOnDefaultScenario fixture's Default panics, \
             so a non-panicking result would mean the entry bypassed Default."
        );
    }
}
