//! Scenario case host (P3).
//!
//! The simulation is the case host. [`Project::run_scenario_simulation`]
//! already owns the full supervised native-execution lifecycle:
//! simulator provisioning, bundle assembly, supervisor admission,
//! native completion, bounded cleanup, and `SimulationRunReport`
//! surface. This module bridges that lifecycle into the SDK
//! scenario shape:
//!
//!  1. Resolve the robot project from the working directory the
//!     harness binary was launched in.
//!  2. Translate the planned scenario's [`ScenarioPlan`] into
//!     [`SimulationRunOptions`] (scene path, finite bound derived
//!     from the plan's transition count, identity, timeouts).
//!  3. Drive [`Project::run_scenario_simulation`]; the simulation lifecycle
//!     admits the supervisor, activates its virtual scenario producer,
//!     publishes the controlled execution, observes the native quantum,
//!     and reaps children.
//!  4. Translate the [`SimulationRunReport`] into lifecycle-owned
//!     [`TerminalEvidence`] for the planned scenario's program. The
//!     seal then validates the lifecycle-observed quantum and
//!     completed-transition count against the program.
//!  5. Return the [`ScenarioRun`] sealed by the case host.
//!
//! The case host never fabricates receipts. A missing simulator, a
//! refused admission, an unfinished supervisor startup, or a
//! nonzero simulator exit code all surface as [`crate::Error`]; the
//! downstream seal refuses a passing verdict.

use std::path::Path;

use phoxal::scenario::{
    Action, Capture, CaptureRecord, CommandReply, EvidenceCollector, PlannedScenario, Program,
    Quantum, ScenarioRun, ScheduleEntry, StepOutcome,
};

use crate::cargo::CargoOptions;
use crate::simulation::{SimulationBound, SimulationPresentation, SimulationRunOptions};
use crate::{Error, Project};

/// Rover quantum in nanoseconds. The plan's `transition_count` is
/// computed from `duration / ROVER_QUANTUM_NANOS` and the supervisor
/// validates the simulator's probed quantum against this constant
/// before admitting the bundle.
pub const ROVER_QUANTUM_NANOS: u64 = 2_000_000;
const ROVER_QUANTUM_MICROS: u32 = 2_000;

/// Drive one planned scenario end-to-end through the simulation
/// lifecycle. Returns the case-host-sealed [`ScenarioRun`]. The
/// caller (the harness binary's `run` mode) is responsible for
/// invoking `verify_box` on the retained scenario instance with this
/// sealed run, then reporting the verdict upward.
pub fn run_case_host(
    planned: &PlannedScenario,
    cargo_options: &CargoOptions,
) -> Result<ScenarioRun, Error> {
    let plan = &planned.plan;
    let scenario_name = planned.name.clone();

    // 1. Resolve the project from the harness binary's working
    //    directory. `cargo-phoxal simulation scenario run` invokes
    //    the harness binary from the robot workspace root, so `.`
    //    resolves to the robot project root.
    let project = Project::discover(Path::new(".")).map_err(|error| Error::SimulationInvalid {
        message: format!(
            "scenario `{scenario_name}` cannot resolve its robot project from the current \
                 working directory: {error}"
        ),
    })?;

    // 2. Translate the plan into `SimulationRunOptions`. The bound
    //    is expressed in quantum-aligned transitions (Steps) so the
    //    simulator's probed quantum does not have to be known up
    //    front — the supervisor's bundle admission enforces the
    //    quantum match. The duration in `SimulationBound::Steps` is
    //    not consulted; the simulator runs `transition_count`
    //    quanta and reports back the actual quantum it observed via
    //    the supervisor's `quantum_ns` validation seam.
    //
    //    Phase-specific quantum constants live in the rover-owned scene
    //    or scenario test data, not the case host. The case host
    //    currently drives the rover fixture, so the 2 ms rover quantum
    //    remains here; a future fixture brings its own quantum.
    let quantum =
        Quantum::from_micros(ROVER_QUANTUM_MICROS).ok_or_else(|| Error::SimulationInvalid {
            message: format!(
                "scenario `{scenario_name}` cannot construct quantum \
                 {ROVER_QUANTUM_MICROS} µs; Quantum::from_micros returned None"
            ),
        })?;
    let transition_count =
        plan.transition_count(quantum)
            .ok_or_else(|| Error::SimulationInvalid {
                message: format!(
                    "scenario `{scenario_name}` plan declares zero transitions or an overflowing \
                 duration at quantum {ROVER_QUANTUM_NANOS} ns; refusing to launch an empty or \
                 overflowing supervised experiment"
                ),
            })?;
    let entries = plan
        .steps
        .iter()
        .map(|step| ScheduleEntry::at(step.boundary, step.action.clone()))
        .collect();
    let program = Program::normalize(
        &scenario_name,
        quantum,
        plan.duration,
        entries,
        plan.captures.clone(),
    )
    .map_err(|error| Error::SimulationInvalid {
        message: format!(
            "scenario `{scenario_name}` cannot normalize the program from its plan: {error}"
        ),
    })?;
    let bound = SimulationBound::Steps(u64::from(transition_count));
    let scene_path = plan.scene.clone();
    let presentation = if std::env::var_os("PHOXAL_SCENARIO_HEADLESS").is_some() {
        SimulationPresentation::Headless
    } else {
        SimulationPresentation::Desktop
    };
    let mut simulation_request = SimulationRunOptions::new(scene_path, presentation, bound)
        .map_err(|error| Error::SimulationInvalid {
            message: format!(
                "scenario `{scenario_name}` cannot construct SimulationRunOptions: {error}"
            ),
        })?
        .with_identity("scenarios", "case-host", sanitize_run_id(&scenario_name))
        .with_auto_run();
    if let Some(executable) = std::env::var_os("PHOXAL_SIMULATOR_EXECUTABLE") {
        simulation_request = simulation_request.with_simulator_executable(executable);
    }

    // 3. Drive the simulation lifecycle. The supervisor owns the virtual
    //    scenario producer and controlled transport; the simulator owns
    //    native completion; the project lifecycle owns bounded cleanup.
    let report = project
        .run_scenario_simulation(cargo_options, &simulation_request, &program)
        .map_err(|error| Error::SimulationInvalid {
            message: format!("scenario `{scenario_name}` supervised execution failed: {error}"),
        })?;

    // 4. Lifecycle-owned terminal evidence. The supervisor's bundle
    //    admission validates `simulation.quantum_ns ==
    //    ROVER_QUANTUM_NANOS` before any child starts (see
    //    `framework/supervisor/src/runtime/mod.rs:run`); a passing
    //    simulation therefore observed the program quantum. The
    //    lifecycle-observed completed transitions equal the plan's
    //    transition count when the simulator exits successfully
    //    (the bound drove the simulator to that boundary). When the
    //    simulator exits non-zero, the seal will reject the run
    //    regardless of the values we record here; the lifecycle flags
    //    below carry the actual supervisor / cleanup outcome.
    //
    //    Construction goes through `TerminalEvidenceBuilder` so the
    //    lifecycle-observed quantum and completed transitions reach
    //    the seal via the typed builder path; absent those calls the
    //    builder would default to zero and the seal's mismatch check
    //    would fail closed.
    let terminal = report
        .terminal
        .as_ref()
        .ok_or_else(|| Error::SimulationInvalid {
            message: format!("scenario `{scenario_name}` produced no native terminal evidence"),
        })?;
    let program_quantum_ns = terminal.quantum_ns;
    let program_transition_count = terminal.completed_steps;
    let final_observation_cut =
        report.provider_contract_verified && terminal.completed_steps == terminal.requested_steps;
    let final_capture_drain = final_observation_cut && report.supervisor_ready;
    let cleanup_ok = report.cleanup.error.is_none();
    let execution_identity = terminal.execution_id.clone();

    // 5. Translate supervisor-observed records and seal the case-host run.
    let mut collector = EvidenceCollector::for_program(program);
    record_scenario_evidence(&mut collector, plan, &report, &scenario_name)?;
    let mut builder = collector
        .terminal_evidence_builder()
        .with_execution_identity(execution_identity)
        .with_terminal_quantum_ns(program_quantum_ns)
        .with_completed_transitions(program_transition_count);
    if final_observation_cut {
        builder = builder.final_observation_cut_observed();
    }
    if final_capture_drain {
        builder = builder.final_capture_drain_observed();
    }
    if cleanup_ok {
        builder = builder.cleanup_succeeded();
    }
    let evidence = builder.build();
    collector
        .record_terminal_evidence(evidence)
        .map_err(|error| Error::SimulationInvalid {
            message: format!("scenario `{scenario_name}` refused its terminal evidence: {error}"),
        })?;
    collector.seal().map_err(|error| Error::SimulationInvalid {
        message: format!("scenario `{scenario_name}` seal refused the run: {error}"),
    })
}

fn record_scenario_evidence(
    collector: &mut EvidenceCollector,
    plan: &phoxal::scenario::ScenarioPlan,
    report: &crate::simulation::SimulationRunReport,
    scenario_name: &str,
) -> Result<(), Error> {
    let scenario = report
        .scenario
        .as_ref()
        .ok_or_else(|| Error::SimulationInvalid {
            message: format!(
                "scenario `{scenario_name}` produced no supervisor execution evidence"
            ),
        })?;
    for observed in &scenario.steps {
        let step_index = collector
            .program()
            .steps()
            .iter()
            .position(|step| step.label == observed.label)
            .ok_or_else(|| Error::SimulationInvalid {
                message: format!(
                    "scenario `{scenario_name}` observed undeclared step `{}`",
                    observed.label
                ),
            })?;
        let step = plan
            .steps
            .get(step_index)
            .ok_or_else(|| Error::SimulationInvalid {
                message: format!(
                    "scenario `{scenario_name}` observed step `{}` without an authored action",
                    observed.label
                ),
            })?;
        let outcome = match &step.action {
            Action::Setpoint { .. } => StepOutcome::SetpointDelivered {
                production: observed.production_boundary,
                eligibility: observed.eligible_boundary,
            },
            Action::Withdraw { .. } => StepOutcome::WithdrawAccepted,
            Action::Command {
                label,
                simulated_deadline,
                host_deadline,
                ..
            } => StepOutcome::CommandIssued {
                label: label.clone(),
                reply_pending: !scenario.command_replies.contains_key(label),
                simulated_deadline_boundary: observed.production_boundary.saturating_add(
                    u64::try_from(simulated_deadline.as_nanos() / u128::from(ROVER_QUANTUM_NANOS))
                        .unwrap_or(u64::MAX),
                ),
                host_deadline_unix_micros: u64::try_from(host_deadline.as_micros())
                    .unwrap_or(u64::MAX),
            },
        };
        collector
            .record_step_outcome(observed.label.clone(), outcome)
            .map_err(|error| Error::SimulationInvalid {
                message: format!(
                    "scenario `{scenario_name}` refused observed step `{}`: {error}",
                    observed.label
                ),
            })?;
    }
    for capture in &scenario.captures {
        let record = match capture.kind.as_str() {
            "state" => CaptureRecord::State(capture.payloads.last().cloned().ok_or_else(|| {
                Error::SimulationInvalid {
                    message: format!(
                        "scenario `{scenario_name}` state capture `{}` was empty",
                        capture.name
                    ),
                }
            })?),
            "sample" => CaptureRecord::Samples(capture.payloads.clone()),
            "event" => CaptureRecord::Events(capture.payloads.clone()),
            kind => {
                return Err(Error::SimulationInvalid {
                    message: format!(
                        "scenario `{scenario_name}` returned unsupported capture kind `{kind}`"
                    ),
                });
            }
        };
        collector
            .record_capture(capture.name.clone(), record)
            .map_err(|error| Error::SimulationInvalid {
                message: format!(
                    "scenario `{scenario_name}` refused capture `{}`: {error}",
                    capture.name
                ),
            })?;
    }
    for (label, payload) in &scenario.command_replies {
        collector
            .record_command_reply(
                label.clone(),
                CommandReply::Accepted {
                    response_bytes: payload.clone(),
                },
            )
            .map_err(|error| Error::SimulationInvalid {
                message: format!(
                    "scenario `{scenario_name}` refused command reply `{label}`: {error}"
                ),
            })?;
    }
    for capture in &plan.captures {
        if let Capture::NativeBody { name, .. } = capture {
            let terminal = report.terminal.as_ref().ok_or_else(|| Error::SimulationInvalid {
                message: format!(
                    "scenario `{scenario_name}` native body capture `{name}` has no terminal evidence"
                ),
            })?;
            let payload = serde_json::to_vec(&terminal.native_body).map_err(|error| {
                Error::SimulationInvalid {
                    message: format!(
                        "scenario `{scenario_name}` cannot encode native body capture `{name}`: {error}"
                    ),
                }
            })?;
            collector
                .record_capture(name.clone(), CaptureRecord::NativeBody(payload))
                .map_err(|error| Error::SimulationInvalid {
                    message: format!(
                        "scenario `{scenario_name}` refused native body capture `{name}`: {error}"
                    ),
                })?;
        }
    }
    Ok(())
}

/// Compose a `run_id` identifier for the supervisor identity. The
/// supervisor validates `run_id` against the lowercase / digit /
/// `-` / `_` rule, so any non-conforming characters are stripped.
fn sanitize_run_id(scenario_name: &str) -> String {
    let mut out = String::with_capacity(scenario_name.len());
    for byte in scenario_name.bytes() {
        let accept =
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_');
        out.push(if accept { byte as char } else { '-' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn run_id_sanitizer_keeps_compatible_bytes() {
        // Uppercase letters and non-allowed punctuation become '-';
        // allowed lowercase letters, digits, '-', '_' pass through.
        assert_eq!(
            sanitize_run_id("scenarios/ForwardTurnStop"),
            "scenarios--orward-urn-top"
        );
        assert_eq!(
            sanitize_run_id("scenarios_with-mixed.Chars"),
            "scenarios_with-mixed--hars"
        );
    }

    #[test]
    fn rover_quantum_matches_2_ms() {
        assert_eq!(ROVER_QUANTUM_NANOS, 2_000_000);
        assert_eq!(
            Duration::from_nanos(ROVER_QUANTUM_NANOS),
            Duration::from_micros(u64::from(ROVER_QUANTUM_MICROS))
        );
    }
}
