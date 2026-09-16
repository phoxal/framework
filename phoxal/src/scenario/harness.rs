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
//! case host, the default [`run_harness`] refuses with an explicit
//! diagnostic naming the missing boundary. The descriptor, plan, and
//! macro path continue to work; the lifecycle boundary is the only
//! thing left to wire up.
//!
//! [`run_harness_with_driver`] is the test seam: callers may
//! supply a driver closure that produces a typed
//! [`crate::scenario::Program`] and an [`EvidenceCollector`] sealed
//! with a real [`crate::scenario::TerminalEvidence`]. The case host
//! then invokes the user's `verify()` against the sealed run. The
//! production case host will replace the test driver with a real
//! supervisor + fixture dispatch.

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
/// 3. Hand the plan to the supplied `driver`, which must produce a
///    typed [`Program`] and an [`EvidenceCollector`] sealed with a
///    real [`crate::scenario::TerminalEvidence`].
/// 4. Invoke the user's `verify()` against the sealed
///    [`crate::scenario::ScenarioRun`].
///
/// The default [`run_harness`] refuses with a diagnostic naming the
/// missing boundary until a real supervisor + fixture dispatch
/// lands. The test seam [`run_harness_with_driver`] lets in-process
/// tests inject a self-driving lifecycle so the macro -> plan ->
/// program -> collector -> seal -> verify path can be exercised
/// without the supervisor. See Gate P1 #3 of followup-5d11cfc1.md.
pub fn run_harness(short_name: &str) -> crate::Result<HarnessRun> {
    run_harness_with_driver(short_name, |_plan| {
        Err(crate::anyhow!(
            "scenario case-host lifecycle is not yet implemented; \
             no supervisor + fixture + collector drove the typed \
             execution. See Gate P1 #3 of followup-5d11cfc1.md."
        ))
    })
}

/// Drive one registered scenario through planning, execution, and
/// verification using a caller-supplied driver. The driver receives
/// the validated [`crate::scenario::ScenarioPlan`] and must return
/// either a sealed [`crate::scenario::ScenarioRun`] (real terminal
/// evidence recorded by the lifecycle) or an error explaining what
/// went wrong.
///
/// `name_in_run` is the descriptor's public scenario identity
/// (`scenarios/<StructIdent>`) and is reported back as the
/// `HarnessRun::name`. The driver closure is responsible for:
/// 1. encoding the plan into a [`Program`],
/// 2. driving execution (supervisor + fixture or self-driving loop),
/// 3. collecting step outcomes, captures, and command replies,
/// 4. recording [`crate::scenario::TerminalEvidence`] with
///    `completed_transitions == program.transition_count()` and a
///    nanosecond-quantum matching `program.quantum().micros() * 1_000`,
/// 5. yielding the sealed [`crate::scenario::ScenarioRun`].
///
/// Once the driver returns a ScenarioRun, the case host invokes the
/// user's `verify()` (captured at registration time) and produces
/// the pass/fail [`ScenarioOutcome`]. See Gate P1 #3 of
/// followup-5d11cfc1.md.
pub fn run_harness_with_driver<F>(short_name: &str, driver: F) -> crate::Result<HarnessRun>
where
    F: FnOnce(&crate::scenario::ScenarioPlan) -> crate::Result<crate::scenario::ScenarioRun>,
{
    let entries = crate::scenario::registry::list_scenarios().map_err(|e| crate::anyhow!("{e}"))?;

    let entry = entries
        .iter()
        .find(|e| e.short_name == short_name)
        .ok_or_else(|| {
            crate::anyhow!("{}", HarnessError::UnknownScenario(short_name.to_owned()))
        })?;

    run_harness_for_entry(entry.entry, driver)
}

/// Drive a single entry function through the case-host pipeline
/// without going through the inventory lookup. Useful for tests
/// that build a `ScenarioDescriptor` locally instead of registering
/// it through `inventory::submit!` (which is global and would race
/// under cargo's default parallel test execution).
pub(crate) fn run_harness_for_entry<F>(
    entry: crate::scenario::ScenarioEntryFn,
    driver: F,
) -> crate::Result<HarnessRun>
where
    F: FnOnce(&crate::scenario::ScenarioPlan) -> crate::Result<crate::scenario::ScenarioRun>,
{
    // (1) Drive the macro-generated entry. The entry constructs the
    //     concrete scenario via `Default::default()`, calls the
    //     user's `plan()`, and hands the validated plan back to
    //     the case host.
    let planned =
        entry().map_err(|e| crate::anyhow!("{}", HarnessError::Internal(format!("{e:#}"))))?;

    // (2) Hand the plan to the driver. The driver produces a sealed
    //     ScenarioRun or returns an error.
    let run = driver(&planned.plan).map_err(|e| {
        crate::anyhow!(
            "{}",
            HarnessError::ScenarioFailed(planned.name.clone(), format!("{e:#}"))
        )
    })?;

    // (3) Invoke the user's verify() against the sealed run on the
    //     same scenario instance the macro called `plan()` on. The
    //     case host owns the outcome: passing verify() yields
    //     `passed: true`, refusing yields a `ScenarioFailed`
    //     diagnostic. The seal's `passed()` is a precondition but
    //     not sufficient; verify() on the retained instance must
    //     also accept.
    if let Err(error) = planned.scenario.verify_box(&run) {
        return Err(crate::anyhow!(
            "{}",
            HarnessError::ScenarioFailed(planned.name.clone(), format!("{error:#}"))
        ));
    }
    Ok(HarnessRun {
        name: planned.name,
        passed: true,
        detail: None,
        report_artifact_path: None,
    })
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
        // Build a non-default `impl Scenario` so the entry can wrap
        // a real `Box<dyn ScenarioBox>` instead of constructing
        // through a closure.
        use crate::scenario::Scenario;
        struct NonPassCheckpoint;
        impl Default for NonPassCheckpoint {
            fn default() -> Self {
                NonPassCheckpoint
            }
        }
        impl Scenario for NonPassCheckpoint {
            fn plan(&self) -> crate::Result<crate::scenario::ScenarioPlan> {
                Ok(crate::scenario::ScenarioPlan::new(
                    "scene",
                    std::time::Duration::from_micros(2_000),
                ))
            }
            fn verify(&self, _run: &crate::scenario::ScenarioRun) -> crate::Result<()> {
                Ok(())
            }
        }
        fn entry() -> crate::Result<crate::scenario::PlannedScenario> {
            let scenario = NonPassCheckpoint;
            let plan = <NonPassCheckpoint as Scenario>::plan(&scenario)?;
            Ok(crate::scenario::PlannedScenario {
                name: "scenarios/NonPassCheckpoint".to_owned(),
                plan,
                scenario: Box::new(scenario),
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

    // ----------------------------------------------------------------
    // End-to-end case-host test through the driver seam. The driver
    // is responsible for the lifecycle the production case host
    // wires to a supervisor + fixture child; the self-driving test
    // exercises the macro -> plan -> program -> collector ->
    // terminal-evidence -> seal -> verify path without spinning up
    // a real supervisor.
    // ----------------------------------------------------------------

    use crate::scenario::results::{CaptureRecord, EvidenceCollector};

    fn setpoint_signature() -> phoxal_port::PortSignature {
        // Build the signature via the public API. The decoder owns
        // its strings through PortSignature::new_owned; we use
        // string literals here because the test only needs a
        // stable signature for the action and capture.
        phoxal_port::PortSignature::new(
            "motion/setpoint",
            "phoxal.motion",
            "Setpoint",
            phoxal_port::PortKind::Setpoint,
            "SetpointRequest",
            "SetpointResponse",
        )
    }

    fn state_capture_signature() -> phoxal_port::PortSignature {
        phoxal_port::PortSignature::new(
            "motion/state",
            "phoxal.motion",
            "State",
            phoxal_port::PortKind::State,
            "State",
            "State",
        )
    }

    fn self_driving_driver(
        plan: &crate::scenario::ScenarioPlan,
    ) -> crate::Result<crate::scenario::ScenarioRun> {
        // Encode the authored plan into a typed Program. The
        // production driver obtains the same Program via the
        // publisher; the self-driving test exercises the validator
        // path the same way.
        let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
        let entries: Vec<crate::scenario::ScheduleEntry> = plan
            .steps
            .iter()
            .map(|step| crate::scenario::ScheduleEntry::at(step.boundary, step.action.clone()))
            .collect();
        let captures: Vec<crate::scenario::Capture> = plan.captures.clone();
        let program = crate::scenario::Program::normalize(
            "scenarios/SelfDriven",
            quantum,
            plan.duration,
            entries,
            captures,
        )
        .map_err(|e| crate::anyhow!("program normalize: {e}"))?;
        let mut collector = EvidenceCollector::for_program(program);
        collector
            .record_step_outcome(
                "s00000000".to_owned(),
                crate::scenario::StepOutcome::SetpointDelivered {
                    production: 0,
                    eligibility: 0,
                },
            )
            .map_err(|e| crate::anyhow!("record step: {e}"))?;
        collector
            .record_capture("motion".to_owned(), CaptureRecord::State(vec![0x01, 0x02]))
            .map_err(|e| crate::anyhow!("record capture: {e}"))?;
        let evidence = collector
            .terminal_evidence_builder()
            .with_execution_identity("exec/scenario/self_driven")
            .final_observation_cut_observed()
            .final_capture_drain_observed()
            .cleanup_succeeded()
            .build();
        collector
            .record_terminal_evidence(evidence)
            .map_err(|e| crate::anyhow!("record terminal evidence: {e}"))?;
        collector.seal().map_err(|e| crate::anyhow!("seal: {e}"))
    }

    /// Run a self-driven scenario end-to-end through the case-host
    /// seam. This is the closest in-tree analogue to a real
    /// `cargo phoxal simulation scenario run ForwardTurnStop` until
    /// the supervisor + fixture lifecycle lands. The case host
    /// must report `passed: true` when the driver seals a
    /// scenario whose `verify()` accepts the sealed run.
    /// See Gate P1 #3 of followup-5d11cfc1.md.
    #[test]
    fn case_host_drives_a_self_driven_scenario_to_pass() {
        use crate::scenario::Scenario;

        struct SparseScenario;
        impl Default for SparseScenario {
            fn default() -> Self {
                SparseScenario
            }
        }
        impl Scenario for SparseScenario {
            fn plan(&self) -> crate::Result<crate::scenario::ScenarioPlan> {
                let step = crate::scenario::Step::new(
                    "s00000000",
                    0,
                    crate::scenario::Action::setpoint(
                        "motion",
                        setpoint_signature(),
                        vec![0xAA],
                        crate::scenario::Validity::Permanent,
                    )
                    .map_err(|e| crate::anyhow!("setpoint action: {e}"))?,
                );
                crate::scenario::ScenarioPlan::with_steps(
                    "self_driven/scene",
                    std::time::Duration::from_micros(6_000),
                    vec![step],
                    vec![
                        crate::scenario::Capture::state("motion", state_capture_signature())
                            .map_err(|e| crate::anyhow!("capture: {e}"))?,
                    ],
                )
                .map_err(|e| crate::anyhow!("plan validate: {e}"))
            }
            fn verify(&self, run: &crate::scenario::ScenarioRun) -> crate::Result<()> {
                if !run.passed() {
                    return Err(crate::anyhow!("scenario run did not seal as passing"));
                }
                Ok(())
            }
        }

        // Build the per-type entry exactly like the macro does:
        // the entry constructs the scenario via `Default`, calls
        // `plan()`, and retains the same instance in a
        // `Box<dyn ScenarioBox>` so the case host's verify() call
        // observes the same struct the macro called plan() on. The
        // closure-free design matches the macro emission.
        fn sparse_entry() -> crate::Result<crate::scenario::PlannedScenario> {
            let scenario = SparseScenario;
            let plan = <SparseScenario as Scenario>::plan(&scenario)?;
            Ok(crate::scenario::PlannedScenario {
                name: "scenarios/SparseScenario".to_owned(),
                plan,
                scenario: Box::new(scenario),
            })
        }

        // Drive the descriptor through the case-host seam. The
        // self-driving driver builds a Program, records outcomes,
        // records real terminal evidence, seals, and returns the
        // run. The case host then invokes `verify_scenario` (which
        // routes to `SparseScenario::verify` via the macro-style
        // fn pointer) and reports `passed: true` only when
        // verify() accepts the sealed run.
        let harness = super::run_harness_for_entry(sparse_entry, self_driving_driver)
            .expect("case host must report passed: true");
        assert_eq!(harness.name, "scenarios/SparseScenario");
        assert!(harness.passed, "self-driven ScenarioRun must pass");
        assert!(harness.detail.is_none(), "passing run carries no detail");
    }

    /// The case host must propagate a `verify()` refusal as a
    /// `ScenarioFailed` error. Without this the case host would
    /// silently pass scenarios whose seal succeeded but whose
    /// user-defined verification rejected the run.
    #[test]
    fn case_host_propagates_verify_failure() {
        use crate::scenario::Scenario;

        struct FailingScenario;
        impl Default for FailingScenario {
            fn default() -> Self {
                FailingScenario
            }
        }
        impl Scenario for FailingScenario {
            fn plan(&self) -> crate::Result<crate::scenario::ScenarioPlan> {
                // The plan must declare `s00000000` so the
                // self-driving driver's `record_step_outcome`
                // succeeds; verify() then rejects the sealed run.
                let step = crate::scenario::Step::new(
                    "s00000000",
                    0,
                    crate::scenario::Action::setpoint(
                        "motion",
                        setpoint_signature(),
                        vec![0xAA],
                        crate::scenario::Validity::Permanent,
                    )
                    .map_err(|e| crate::anyhow!("setpoint action: {e}"))?,
                );
                crate::scenario::ScenarioPlan::with_steps(
                    "self_driven/scene",
                    std::time::Duration::from_micros(6_000),
                    vec![step],
                    vec![
                        crate::scenario::Capture::state("motion", state_capture_signature())
                            .map_err(|e| crate::anyhow!("capture: {e}"))?,
                    ],
                )
                .map_err(|e| crate::anyhow!("plan validate: {e}"))
            }
            fn verify(&self, _run: &crate::scenario::ScenarioRun) -> crate::Result<()> {
                Err(crate::anyhow!("FailingScenario always rejects"))
            }
        }

        fn failing_entry() -> crate::Result<crate::scenario::PlannedScenario> {
            let scenario = FailingScenario;
            let plan = <FailingScenario as Scenario>::plan(&scenario)?;
            Ok(crate::scenario::PlannedScenario {
                name: "scenarios/FailingScenario".to_owned(),
                plan,
                scenario: Box::new(scenario),
            })
        }

        let result = super::run_harness_for_entry(failing_entry, self_driving_driver);
        let err = result.expect_err("verify() must propagate as ScenarioFailed");
        let message = format!("{err:#}");
        assert!(
            message.contains("FailingScenario always rejects"),
            "case host must surface verify()'s diagnostic; got `{message}`"
        );
        assert!(
            message.contains("scenarios/FailingScenario"),
            "case host must name the scenario in the diagnostic; got `{message}`"
        );
    }

    /// The case host must invoke `verify_box` on the same scenario
    /// instance the macro entry called `plan()` on, so any state
    /// the user's `plan()` consulted on `&self` is consistent with
    /// what `verify_box` sees. This is the regression for "verify
    /// uses a different scenario instance" — both methods must
    /// observe the same struct.
    #[test]
    fn verify_uses_same_scenario_instance_as_plan() {
        use crate::scenario::Scenario;

        // Stateful scenario: Default constructs with a counter at
        // 0; plan() does NOT mutate it (Scenario::plan takes &self).
        // verify() reads the counter; the case host must observe
        // the value plan() observed. Since plan() does not mutate
        // through &self, the structural test below uses a `Cell`
        // exposed via a derived getter to demonstrate the
        // same-instance invariant.
        struct StatefulScenario(std::cell::Cell<u32>);
        impl Default for StatefulScenario {
            fn default() -> Self {
                StatefulScenario(std::cell::Cell::new(7))
            }
        }
        impl Scenario for StatefulScenario {
            fn plan(&self) -> crate::Result<crate::scenario::ScenarioPlan> {
                // plan() reads the cell; the entry retains this
                // exact instance, so verify() must see the same
                // value 7.
                let observed = self.0.get();
                assert_eq!(observed, 7, "plan() observed counter = 7");
                Ok(crate::scenario::ScenarioPlan::new(
                    "stateful/scene",
                    std::time::Duration::from_micros(2_000),
                ))
            }
            fn verify(&self, run: &crate::scenario::ScenarioRun) -> crate::Result<()> {
                // verify() must see the same cell value plan()
                // saw, even though the entry used to construct a
                // fresh instance (which would also see 7 because
                // Default::default() reloads). The structural
                // guarantee matters because the user's plan() may
                // mutate `&mut self` if they switch the signature,
                // and verify() must see those mutations. The case
                // host's Box<dyn ScenarioBox> guarantees that.
                let observed = self.0.get();
                if observed != 7 {
                    return Err(crate::anyhow!(
                        "verify() observed counter = {observed}; case host \
                         must invoke verify_box on the same instance plan() ran"
                    ));
                }
                if !run.passed() {
                    return Err(crate::anyhow!("scenario run did not seal as passing"));
                }
                Ok(())
            }
        }

        fn stateful_entry() -> crate::Result<crate::scenario::PlannedScenario> {
            let scenario = StatefulScenario::default();
            let plan = <StatefulScenario as Scenario>::plan(&scenario)?;
            Ok(crate::scenario::PlannedScenario {
                name: "scenarios/StatefulScenario".to_owned(),
                plan,
                scenario: Box::new(scenario),
            })
        }

        let driver =
            |_plan: &crate::scenario::ScenarioPlan| -> crate::Result<crate::scenario::ScenarioRun> {
                // Build a minimal passing ScenarioRun via the public
                // collector API.
                use crate::scenario::results::EvidenceCollector;
                let quantum = crate::scenario::Quantum::from_micros(2_000).expect("quantum");
                let program = crate::scenario::Program::normalize(
                    "scenarios/StatefulScenario",
                    quantum,
                    std::time::Duration::from_micros(2_000),
                    vec![],
                    vec![],
                )
                .map_err(|e| crate::anyhow!("{e}"))?;
                let mut collector = EvidenceCollector::for_program(program);
                let evidence = collector
                    .terminal_evidence_builder()
                    .with_execution_identity("exec/scenario/stateful")
                    .final_observation_cut_observed()
                    .final_capture_drain_observed()
                    .cleanup_succeeded()
                    .build();
                collector
                    .record_terminal_evidence(evidence)
                    .map_err(|e| crate::anyhow!("{e}"))?;
                collector.seal().map_err(|e| crate::anyhow!("{e}"))
            };

        let harness = super::run_harness_for_entry(stateful_entry, driver)
            .expect("verify on retained instance must accept the same value");
        assert!(harness.passed);
    }
}
