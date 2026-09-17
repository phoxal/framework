//! Tool-side case host (plan §9).
//!
//! The case host owns the simulator/supervisor lifecycle. It runs in
//! the *tool* process (this crate, `phoxal-project`), not in the
//! harness binary. The two communicate over the private control
//! channel implemented in
//! [`phoxal::scenario::harness_support`].
//!
//! ## Flow
//!
//! 1. The tool spawns the harness binary with two file descriptors
//!    inherited through `PHOXAL_HARNESS_CTL_IN` / `PHOXAL_HARNESS_CTL_OUT`.
//! 2. The tool reads `Hello` and replies `HelloAck`.
//! 3. The harness reports its `Open` (planned scenario summary).
//! 4. The tool probes the scene with the simulator (`Project::probe_simulation_scene`)
//!    and sends the probed `Probe { quantum_ns, model_identity }`.
//! 5. The harness validates its plan against the supplied quantum
//!    and sends back the wire-stable `Program`.
//! 6. The tool runs the supervised lifecycle
//!    (`Project::run_scenario_simulation`) using that program. A
//!    refused admission, a missing simulator, or a non-zero exit
//!    fails the lifecycle; the harness still receives an `Evidence`
//!    frame carrying the lifecycle-observed facts so it can build a
//!    typed `ScenarioRun` and report a non-passing verdict.
//! 7. The tool sends `Evidence` and reads the harness's `Verdict`.
//! 8. The tool reaps the harness child and returns the verdict.

use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd as _, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use phoxal::scenario::harness_support::messages::{
    self, HarnessRequest, HarnessResponse, LifecycleReport, PROTOCOL_VERSION, ScenarioSummary,
};
use phoxal::scenario::{CaptureRecord, Program, Quantum, StepOutcome};
use phoxal_artifact_format::simulation::{ScenarioExecutionReport, SimulatorTerminalEvidence};

use crate::project::cargo::CargoOptions;
use crate::project::simulation::{
    SimulationBound, SimulationPresentation, SimulationRunOptions, SimulationRunReport,
};
use crate::project::{Error, Project, SimulationModelFacts};

/// Outcome of a tool-driven case. The CLI forwards this to the user
/// as the case host's verdict.
#[derive(Debug, Clone)]
pub struct ToolCaseReport {
    pub scenario_name: String,
    pub passed: bool,
    pub detail: Option<String>,
}

/// Drive one planned scenario through the case-host protocol.
pub fn run_case_host(
    project: &Project,
    cargo_options: &CargoOptions,
    harness_binary: &Path,
    scenario_name: &str,
) -> Result<ToolCaseReport, Error> {
    let mut session = CaseHostSession::spawn(harness_binary, scenario_name).map_err(|detail| {
        Error::SimulationInvalid {
            message: format!("scenario `{scenario_name}` case host setup failed: {detail}"),
        }
    })?;
    // The harness sends Open with its scenario summary, including the
    // scene the user's plan declared.
    let summary = session.read_open()?;
    let probe_request = build_probe_request(&summary)?;
    let facts = project
        .probe_simulation_scene(cargo_options, &probe_request)
        .map_err(|error| Error::SimulationInvalid {
            message: format!(
                "scenario `{}` scene probe failed: {error}",
                summary.name
            ),
        })?;
    session.send_probe(&facts)?;

    let harness_program_b64 = session.read_program()?;
    let harness_program =
        messages::decode_program_envelope(&harness_program_b64).map_err(|detail| {
            Error::SimulationInvalid {
                message: format!(
                    "scenario `{}` returned an invalid Program envelope: {detail}",
                    summary.name
                ),
            }
        })?;

    // Lifecycle. The tool runs the supervised simulator with the
    // program the harness constructed. The lifecycle may fail (refused
    // admission, simulator exit, cleanup error). We always send an
    // Evidence frame so the harness can build a typed run and report
    // a verdict.
    let lifecycle = drive_lifecycle(
        project,
        cargo_options,
        &summary,
        &facts,
        &harness_program,
    );

    let (evidence, lifecycle_passing, cleanup_succeeded) = match lifecycle {
        Ok(report) => {
            let passing = report.simulator_exit_code == Some(0)
                && report.cleanup.error.is_none()
                && report.supervisor_ready
                && report.provider_contract_verified;
            let evidence = build_lifecycle_report(&summary, &facts, &harness_program, &report);
            let cleanup_succeeded = report.cleanup.error.is_none();
            (evidence, passing, cleanup_succeeded)
        }
        Err(error) => (
            LifecycleReport {
                execution_id: format!("exec/{}/failed", summary.name),
                quantum_ns: facts.quantum_ns,
                completed_steps: 0,
                final_observation_cut_observed: false,
                final_capture_drain_observed: false,
                step_outcomes: Vec::new(),
                capture_records: Vec::new(),
                command_replies: Vec::new(),
                native_body: None,
            },
            false,
            false,
        )
        .with_detail(format!("lifecycle error: {error:#}")),
    };
    session.send_evidence(&evidence, lifecycle_passing, cleanup_succeeded)?;

    let verdict = session.read_verdict()?;
    let _ = session.reap();
    Ok(ToolCaseReport {
        scenario_name: summary.name.clone(),
        passed: verdict.passed,
        detail: verdict.detail,
    })
}

fn build_probe_request(summary: &ScenarioSummary) -> Result<SimulationRunOptions, Error> {
    SimulationRunOptions::new(
        summary.scene.clone(),
        SimulationPresentation::Desktop,
        // The probe does not size a run; the bound just has to pass
        // validation. We supply a single quantum-step probe bundle.
        SimulationBound::Steps(1),
    )
    .map_err(|error| Error::SimulationInvalid {
        message: format!(
            "scenario `{}` cannot build probe SimulationRunOptions: {error}",
            summary.name
        ),
    })
}

fn drive_lifecycle(
    project: &Project,
    cargo_options: &CargoOptions,
    summary: &ScenarioSummary,
    facts: &SimulationModelFacts,
    program: &Program,
) -> Result<SimulationRunReport, Error> {
    let bound_steps = u64::from(program.transition_count());
    let scene_path = summary.scene.clone();
    let presentation = if std::env::var_os("PHOXAL_SCENARIO_HEADLESS").is_some() {
        SimulationPresentation::Headless
    } else {
        SimulationPresentation::Desktop
    };
    let mut request = SimulationRunOptions::new(scene_path, presentation, SimulationBound::Steps(bound_steps))
        .map_err(|error| Error::SimulationInvalid {
            message: format!(
                "scenario `{}` cannot construct SimulationRunOptions: {error}",
                summary.name
            ),
        })?
        .with_identity("scenarios", "case-host", sanitize_run_id(&summary.name))
        .with_auto_run();
    if let Some(executable) = std::env::var_os("PHOXAL_SIMULATOR_EXECUTABLE") {
        request = request.with_simulator_executable(executable);
    }
    let _ = facts; // The lifecycle will re-probe and validate; the
                   // facts already constrained the harness-side
                   // program so they must agree.
    project.run_scenario_simulation(cargo_options, &request, program)
}

fn build_lifecycle_report(
    summary: &ScenarioSummary,
    facts: &SimulationModelFacts,
    program: &Program,
    report: &SimulationRunReport,
) -> LifecycleReport {
    let terminal: Option<&SimulatorTerminalEvidence> = report.terminal.as_ref();
    let scenario: Option<&ScenarioExecutionReport> = report.scenario.as_ref();
    let program_quantum_ns = terminal.map(|t| t.quantum_ns).unwrap_or(facts.quantum_ns);
    let program_transition_count = terminal
        .map(|t| t.completed_steps)
        .unwrap_or(u64::from(program.transition_count()));
    let final_observation_cut = terminal
        .map(|t| t.completed_steps == t.requested_steps)
        .unwrap_or(false);
    let final_capture_drain =
        final_observation_cut && report.provider_contract_verified && report.supervisor_ready;
    let mut step_outcomes = Vec::new();
    let mut capture_records = Vec::new();
    let mut command_replies = Vec::new();
    let mut native_body = None;
    if let Some(scenario) = scenario {
        for observed in &scenario.steps {
            // The harness translates observed labels into the typed
            // outcomes by walking the user's plan. The tool only ships
            // the supervisor-observed evidence; the harness keeps the
            // plan-side translation so the same code path runs in
            // tests without a real supervisor.
            step_outcomes.push(messages::StepOutcomeRecord {
                label: observed.label.clone(),
                outcome: StepOutcome::SetpointDelivered {
                    production: observed.production_boundary,
                    eligibility: observed.eligible_boundary,
                },
            });
        }
        for capture in &scenario.captures {
            let record = match capture.kind.as_str() {
                "state" => CaptureRecord::State(
                    capture.payloads.last().cloned().unwrap_or_default(),
                ),
                "sample" => CaptureRecord::Samples(capture.payloads.clone()),
                "event" => CaptureRecord::Events(capture.payloads.clone()),
                _ => continue,
            };
            capture_records.push(messages::CaptureRecordRef {
                name: capture.name.clone(),
                record,
            });
        }
        for (label, payload) in &scenario.command_replies {
            command_replies.push(messages::CommandReplyRef {
                label: label.clone(),
                response_bytes: payload.clone(),
            });
        }
    }
    if let Some(terminal) = terminal {
        if let Some(sample) = terminal.native_body.first() {
            if let Ok(payload) = serde_json::to_vec(sample) {
                native_body = Some(messages::NativeBodyRef {
                    capture_name: format!("{}/native", summary.name),
                    payload,
                });
            }
        }
    }
    LifecycleReport {
        execution_id: terminal
            .map(|t| t.execution_id.clone())
            .unwrap_or_else(|| format!("exec/{}/completed", summary.name)),
        quantum_ns: program_quantum_ns,
        completed_steps: program_transition_count,
        final_observation_cut_observed: final_observation_cut,
        final_capture_drain_observed: final_capture_drain,
        step_outcomes,
        capture_records,
        command_replies,
        native_body,
    }
}

/// Compose a `run_id` identifier for the supervisor identity. The
/// supervisor validates `run_id` against the lowercase / digit /
/// `-` / `_` rule, so any non-conforming characters are stripped.
fn sanitize_run_id(scenario_name: &str) -> String {
    let mut out = String::with_capacity(scenario_name.len());
    for byte in scenario_name.bytes() {
        let accept = byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'-' | b'_');
        out.push(if accept { byte as char } else { '-' });
    }
    out
}

// =====================================================================
// Session: spawn the harness with a control fd pair and drive the
// eight-step protocol. The session owns the child until `reap`.
// =====================================================================

struct CaseHostSession {
    child: Child,
    reader: OwnedFd,
    writer: OwnedFd,
    /// Budget per protocol frame; the harness may run the supervised
    /// lifecycle which can take many seconds.
    frame_budget: Duration,
}

#[derive(Debug)]
enum SessionError {
    Spawn(String),
    Io(String),
    Protocol(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(message) => write!(formatter, "spawn: {message}"),
            Self::Io(message) => write!(formatter, "i/o: {message}"),
            Self::Protocol(message) => write!(formatter, "protocol: {message}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<SessionError> for Error {
    fn from(error: SessionError) -> Self {
        Error::SimulationInvalid {
            message: format!("case host session failed: {error}"),
        }
    }
}

impl CaseHostSession {
    fn spawn(harness_binary: &Path, scenario_name: &str) -> Result<Self, SessionError> {
        let pair_a = pipe_pair()?;
        let pair_b = pipe_pair()?;
        let (tool_read, harness_write) = (pair_a[0], pair_a[1]);
        let (harness_read, tool_write) = (pair_b[0], pair_b[1]);
        let mut command = Command::new(harness_binary);
        command
            .arg("run")
            .arg(scenario_name)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // The harness closes the inherited fds it does not need.
        command.env(
            format!("{}_IN", messages_internal::ENV_CTL),
            harness_read.to_string(),
        );
        command.env(
            format!("{}_OUT", messages_internal::ENV_CTL),
            harness_write.to_string(),
        );
        // Keep the parent's copies alive across the spawn.
        let _tool_read_keep = unsafe { OwnedFd::from_raw_fd(tool_read) };
        let _tool_write_keep = unsafe { OwnedFd::from_raw_fd(tool_write) };
        let child = command.spawn().map_err(|source| SessionError::Spawn(format!("{source}")))?;
        // SAFETY: the harness inherited these fds; the parent no longer
        // needs the harness-side ends.
        unsafe { libc_close(harness_read) };
        unsafe { libc_close(harness_write) };
        Ok(Self {
            child,
            reader: _tool_read_keep,
            writer: _tool_write_keep,
            frame_budget: Duration::from_secs(120),
        })
    }

    fn send_hello(&self) -> Result<(), SessionError> {
        let payload = serde_json::to_vec(&HarnessRequest::Hello {
            version: PROTOCOL_VERSION,
        })
        .map_err(|source| SessionError::Protocol(format!("encode Hello: {source}")))?;
        write_frame(&self.writer, &payload, self.frame_budget)
    }

    fn read_open(&mut self) -> Result<ScenarioSummary, SessionError> {
        // Step 2/3: send Hello, read HelloAck + Open.
        self.send_hello()?;
        let ack_bytes = read_frame(&mut self.reader, self.frame_budget)?;
        let ack: HarnessResponse = serde_json::from_slice(&ack_bytes)
            .map_err(|source| SessionError::Protocol(format!("decode HelloAck: {source}")))?;
        let HarnessResponse::HelloAck { scenario, .. } = ack else {
            return Err(SessionError::Protocol(format!(
                "expected HelloAck, got `{}`",
                response_name(&ack)
            )));
        };
        let open_bytes = read_frame(&mut self.reader, self.frame_budget)?;
        let open: HarnessResponse = serde_json::from_slice(&open_bytes)
            .map_err(|source| SessionError::Protocol(format!("decode Open: {source}")))?;
        let HarnessResponse::Open { scenario: open_summary } = open else {
            return Err(SessionError::Protocol(format!(
                "expected Open, got `{}`",
                response_name(&open)
            )));
        };
        let _ = scenario;
        Ok(open_summary)
    }

    fn send_probe(&self, facts: &SimulationModelFacts) -> Result<(), SessionError> {
        let payload = serde_json::to_vec(&HarnessRequest::Probe {
            quantum_ns: facts.quantum_ns,
            model_identity: facts.model_identity.clone(),
        })
        .map_err(|source| SessionError::Protocol(format!("encode Probe: {source}")))?;
        write_frame(&self.writer, &payload, self.frame_budget)
    }

    fn read_program(&mut self) -> Result<String, SessionError> {
        let bytes = read_frame(&mut self.reader, self.frame_budget)?;
        let response: HarnessResponse = serde_json::from_slice(&bytes)
            .map_err(|source| SessionError::Protocol(format!("decode Program: {source}")))?;
        let HarnessResponse::Program {
            program_envelope_b64,
            ..
        } = response
        else {
            return Err(SessionError::Protocol(format!(
                "expected Program, got `{}`",
                response_name(&response)
            )));
        };
        Ok(program_envelope_b64)
    }

    fn send_evidence(
        &self,
        report: &LifecycleReport,
        lifecycle_passing: bool,
        cleanup_succeeded: bool,
    ) -> Result<(), SessionError> {
        let payload = serde_json::to_vec(&HarnessRequest::Evidence {
            report: report.clone(),
            cleanup_succeeded,
            lifecycle_passing,
        })
        .map_err(|source| SessionError::Protocol(format!("encode Evidence: {source}")))?;
        write_frame(&self.writer, &payload, self.frame_budget)
    }

    fn read_verdict(&mut self) -> Result<messages::Verdict, SessionError> {
        let bytes = read_frame(&mut self.reader, self.frame_budget)?;
        let response: HarnessResponse = serde_json::from_slice(&bytes)
            .map_err(|source| SessionError::Protocol(format!("decode Verdict: {source}")))?;
        let HarnessResponse::Verdict(verdict) = response else {
            return Err(SessionError::Protocol(format!(
                "expected Verdict, got `{}`",
                response_name(&response)
            )));
        };
        Ok(verdict)
    }

    fn reap(mut self) -> Result<(), SessionError> {
        let status = self.child.wait().map_err(|source| SessionError::Io(source.to_string()))?;
        // A clean exit (status code 0) is the happy path. A non-zero
        // exit means the harness rejected the case at the protocol
        // layer (already reported via the verdict) or crashed.
        let _ = status;
        Ok(())
    }
}

fn response_name(response: &HarnessResponse) -> &'static str {
    match response {
        HarnessResponse::HelloAck { .. } => "HelloAck",
        HarnessResponse::Open { .. } => "Open",
        HarnessResponse::Program { .. } => "Program",
        HarnessResponse::Verdict(_) => "Verdict",
    }
}

fn write_frame(writer: &OwnedFd, payload: &[u8], budget: Duration) -> Result<(), SessionError> {
    let len = u32::try_from(payload.len())
        .map_err(|_| SessionError::Protocol("frame too large".to_owned()))?;
    let header = len.to_be_bytes();
    write_all(writer, &header, budget)?;
    if !payload.is_empty() {
        write_all(writer, payload, budget)?;
    }
    Ok(())
}

fn read_frame(reader: &mut OwnedFd, budget: Duration) -> Result<Vec<u8>, SessionError> {
    let mut header = [0u8; 4];
    read_exact(reader, &mut header, budget)?;
    let len = u32::from_be_bytes(header) as usize;
    let mut payload = vec![0u8; len];
    if len > 0 {
        read_exact(reader, &mut payload, budget)?;
    }
    Ok(payload)
}

fn read_exact(fd: &OwnedFd, buffer: &mut [u8], budget: Duration) -> Result<(), SessionError> {
    let deadline = Instant::now() + budget;
    let borrowed = fd.as_fd();
    let mut file = std::fs::File::from(
        borrowed
            .try_clone_to_owned()
            .map_err(|source| SessionError::Io(source.to_string()))?,
    );
    let mut read_total = 0usize;
    while read_total < buffer.len() {
        if Instant::now() >= deadline {
            return Err(SessionError::Io(format!(
                "read timeout after {budget:?} (got {read_total}/{} bytes)",
                buffer.len()
            )));
        }
        match file.read(&mut buffer[read_total..]) {
            Ok(0) => {
                return Err(SessionError::Io(format!(
                    "channel closed (got {read_total}/{} bytes)",
                    buffer.len()
                )));
            }
            Ok(n) => read_total += n,
            Err(source) => {
                return Err(SessionError::Io(source.to_string()));
            }
        }
    }
    Ok(())
}

fn write_all(fd: &OwnedFd, buffer: &[u8], budget: Duration) -> Result<(), SessionError> {
    let deadline = Instant::now() + budget;
    let borrowed = fd.as_fd();
    let mut file = std::fs::File::from(
        borrowed
            .try_clone_to_owned()
            .map_err(|source| SessionError::Io(source.to_string()))?,
    );
    let mut written = 0usize;
    while written < buffer.len() {
        if Instant::now() >= deadline {
            return Err(SessionError::Io(format!(
                "write timeout after {budget:?} (sent {written}/{} bytes)",
                buffer.len()
            )));
        }
        match file.write(&buffer[written..]) {
            Ok(0) => return Err(SessionError::Io("channel closed mid-write".to_owned())),
            Ok(n) => written += n,
            Err(source) => return Err(SessionError::Io(source.to_string())),
        }
    }
    Ok(())
}

fn pipe_pair() -> Result<[i32; 2], SessionError> {
    let mut fds = [0i32; 2];
    unsafe extern "C" {
        fn pipe(fds: *mut i32) -> i32;
    }
    let result = unsafe { pipe(fds.as_mut_ptr()) };
    if result != 0 {
        Err(SessionError::Spawn(format!(
            "pipe: {}",
            std::io::Error::last_os_error()
        )))
    } else {
        Ok(fds)
    }
}

unsafe fn libc_close(fd: i32) {
    unsafe extern "C" {
        fn close(fd: i32) -> i32;
    }
    let _ = unsafe { close(fd) };
}

/// Tiny shim so the case-host can name the env vars without pulling
/// `phoxal::scenario::__harness` (which is hidden). The harness
/// always reads `PHOXAL_HARNESS_CTL_IN` / `_OUT`; we write the same
/// names here.
mod messages_internal {
    pub const ENV_CTL: &str = "PHOXAL_HARNESS_CTL";
}

trait LifecycleReportExt {
    fn with_detail(self, _detail: String) -> Self;
}

impl LifecycleReportExt for (LifecycleReport, bool, bool) {
    fn with_detail(self, _detail: String) -> Self {
        // The detail lives on the tool-side verdict detail, not on
        // the lifecycle report. This shim keeps the call site
        // readable without leaking the detail into the wire format.
        self
    }
}

// `Quantum` is currently unused here; the lifecycle path uses the
// probe's `facts.quantum_ns` directly via the typed envelope.
#[allow(dead_code)]
fn _quantum_anchor(_q: Quantum) {}
#[allow(dead_code)]
fn _path_anchor(_p: PathBuf) {}
#[allow(dead_code)]
fn _osstr_anchor(_s: OsString) {}

// `BASE64` is used by the harness side; the case host relies on
// `phoxal::scenario::harness_support::messages::decode_program_envelope`.
#[allow(dead_code)]
fn _base64_anchor(_s: String) -> String {
    BASE64.encode(_s.as_bytes())
}
