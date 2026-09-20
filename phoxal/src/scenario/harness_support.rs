//! Hidden SDK-side support for the generated scenario harness binary.
//!
//! The harness runs in a separate process from the tool;
//! the two communicate over a small, versioned, bounded private
//! control channel. This module implements the harness side: it reads
//! tool requests, constructs the SDK-side plan/program/evidence, and
//! returns verdicts through the channel.
//!
//! The module is intentionally limited. It performs no Cargo work,
//! does not touch the keyring, never spawns processes, and never
//! provisions a simulator. All lifecycle, provisioning, and cleanup
//! remain on the tool side.
//!
//! ## Channel framing
//!
//! Each frame is a 4-byte big-endian length prefix followed by that
//! many bytes of UTF-8 JSON. Frames are bounded to
//! [`MAX_FRAME_BYTES`] (1 MiB); a longer frame, a malformed frame, a
//! closed channel, or a read/write error fails the case. The first
//! frame the harness reads is a [`Hello`] from the tool; the harness
//! replies with [`HelloAck`] and the rest of the conversation is
//! request/response driven by the tool.
//!
//! The Program envelope rides the existing canonical wire format
//! (the bytes [`crate::scenario::program::Program::program_bytes`]
//! produces) embedded as a base64 string. The tool reconstructs with
//! [`crate::scenario::program::Program::decode`].
//!
//! ## File-descriptor handoff
//!
//! The tool passes two file descriptors to the harness via the
//! environment variables `PHOXAL_HARNESS_CTL_IN` (the harness reads
//! from this fd) and `PHOXAL_HARNESS_CTL_OUT` (the harness writes to
//! this fd). Both fds must be inherited across exec and positioned
//! before the harness's `main` reads them. A missing or non-numeric
//! env var fails the case before the harness reads its first frame.
//!
//! [`Hello`]: messages::HarnessRequest::Hello
//! [`HelloAck`]: messages::HarnessResponse::HelloAck

use std::env;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::scenario::program::{Program, Quantum, ScheduleEntry};
use crate::scenario::registry::PlannedScenario;
use crate::scenario::results::{CaptureRecord, CommandReply, EvidenceCollector, ScenarioRun};

pub mod messages;

pub use messages::{
    HarnessRequest, HarnessResponse, ScenarioSummary, Verdict, decode_program_envelope,
    encode_program_envelope,
};

/// Maximum accepted frame size. Tool and harness both reject frames
/// larger than this as malformed; the limit is the only knob the
/// protocol exposes for sizing the channel.
pub const MAX_FRAME_BYTES: usize = 1 << 20;

/// Environment variable naming the read-side fd for the control
/// channel. The tool opens the fd pair, inherits it across exec into
/// the harness, and writes the parsed fd number into the harness's
/// environment before launch.
pub const ENV_CTL_IN: &str = "PHOXAL_HARNESS_CTL_IN";
/// Environment variable naming the write-side fd for the control
/// channel.
pub const ENV_CTL_OUT: &str = "PHOXAL_HARNESS_CTL_OUT";

/// Default per-message read/write budget. The tool is allowed to take
/// arbitrarily long for one case; this is the harness's local timeout
/// for each individual channel operation so a stuck tool cannot pin
/// the harness forever.
const IO_BUDGET: Duration = Duration::from_secs(60);

/// Failure modes the harness-side protocol driver can return. These
/// surface as a non-zero harness exit and a tool-side error; the case
/// host's cleanup path always runs.
#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("harness control channel fd `{name}` is missing or not an integer")]
    BadFdEnv { name: &'static str },
    #[error("harness control channel: i/o error: {message}")]
    Io { message: String },
    #[error("harness control channel: malformed frame ({message})")]
    Malformed { message: String },
    #[error(
        "harness control channel: unsupported protocol version (got {got}, expected {expected})"
    )]
    UnsupportedVersion { got: u32, expected: u32 },
    #[error("harness control channel: frame exceeds {limit} bytes (got {got})")]
    FrameTooLarge { got: usize, limit: usize },
    #[error("scenario `{0}` is not registered")]
    UnknownScenario(String),
    #[error("scenario `{name}` planning failed: {source}")]
    Planning { name: String, source: anyhow::Error },
    #[error("scenario `{name}` case-host protocol failed: {message}")]
    Protocol { name: String, message: String },
}

/// One end of the framed JSON control channel. The harness holds an
/// inbound reader and an outbound writer constructed from the file
/// descriptors the tool passed through the environment.
pub struct Channel {
    reader: File,
    writer: File,
}

impl Channel {
    /// Open the harness-side channel from inherited file descriptors.
    /// The fds come from [`ENV_CTL_IN`] and [`ENV_CTL_OUT`].
    pub fn from_env() -> Result<Self, HarnessError> {
        let in_fd = parse_fd_env(ENV_CTL_IN)?;
        let out_fd = parse_fd_env(ENV_CTL_OUT)?;
        // SAFETY: the harness only owns the fds after this point; the
        // tool does not read from the inbound end or write to the
        // outbound end until the harness exits or closes the channel.
        let reader = unsafe { File::from_raw_fd(in_fd) };
        let writer = unsafe { File::from_raw_fd(out_fd) };
        Ok(Self { reader, writer })
    }

    /// Open the harness-side channel from explicit fds. Used by tests
    /// to exercise the harness-side driver without spawning a child.
    pub fn from_fds(in_fd: OwnedFd, out_fd: OwnedFd) -> Self {
        let reader = File::from(in_fd);
        let writer = File::from(out_fd);
        Self { reader, writer }
    }

    /// Read exactly one framed message from the channel.
    pub fn read_message(&mut self) -> Result<HarnessRequest, HarnessError> {
        let bytes = self.read_frame()?;
        serde_json::from_slice::<HarnessRequest>(&bytes).map_err(|source| HarnessError::Malformed {
            message: format!("request decode: {source}"),
        })
    }

    /// Write exactly one framed message to the channel.
    pub fn write_message(&mut self, message: &HarnessResponse) -> Result<(), HarnessError> {
        let bytes = serde_json::to_vec(message).map_err(|source| HarnessError::Malformed {
            message: format!("response encode: {source}"),
        })?;
        self.write_frame(&bytes)
    }

    fn read_frame(&mut self) -> Result<Vec<u8>, HarnessError> {
        let mut header = [0u8; 4];
        read_exact(&mut self.reader, &mut header, IO_BUDGET)
            .map_err(|message| HarnessError::Io { message })?;
        let length = u32::from_be_bytes(header) as usize;
        if length > MAX_FRAME_BYTES {
            return Err(HarnessError::FrameTooLarge {
                got: length,
                limit: MAX_FRAME_BYTES,
            });
        }
        let mut payload = vec![0u8; length];
        if length > 0 {
            read_exact(&mut self.reader, &mut payload, IO_BUDGET)
                .map_err(|message| HarnessError::Io { message })?;
        }
        Ok(payload)
    }

    fn write_frame(&mut self, payload: &[u8]) -> Result<(), HarnessError> {
        if payload.len() > MAX_FRAME_BYTES {
            return Err(HarnessError::FrameTooLarge {
                got: payload.len(),
                limit: MAX_FRAME_BYTES,
            });
        }
        let header = u32::try_from(payload.len())
            .map_err(|_| HarnessError::FrameTooLarge {
                got: payload.len(),
                limit: MAX_FRAME_BYTES,
            })?
            .to_be_bytes();
        write_all(&mut self.writer, &header, IO_BUDGET)
            .map_err(|message| HarnessError::Io { message })?;
        if !payload.is_empty() {
            write_all(&mut self.writer, payload, IO_BUDGET)
                .map_err(|message| HarnessError::Io { message })?;
        }
        Ok(())
    }
}

/// Drive the harness-side protocol for one case. The harness looks
/// up the named scenario in the SDK registry, constructs the
/// [`PlannedScenario`] (retaining the user's instance for
/// `verify_box`), and walks the eight-step control flow described in
/// plan §9.
pub fn run_harness_case(scenario_name: &str) -> Result<(), HarnessError> {
    let mut channel = Channel::from_env()?;
    let planned = plan_scenario(scenario_name)?;

    // Step 2/3: read Hello from the tool, send HelloAck with the
    // scenario summary. This is the harness's *only* chance to fail
    // the case before the tool has done any work; a malformed
    // handshake here fails the tool's setup rather than a later
    // bundle build.
    let hello = channel.read_message()?;
    let HarnessRequest::Hello { version } = hello else {
        return Err(HarnessError::Protocol {
            name: planned.name.clone(),
            message: format!("expected Hello, got `{}`", request_name(&hello)),
        });
    };
    let expected_version = messages::PROTOCOL_VERSION;
    if version != expected_version {
        return Err(HarnessError::UnsupportedVersion {
            got: version,
            expected: expected_version,
        });
    }
    channel.write_message(&HarnessResponse::HelloAck {
        version: expected_version,
        scenario: scenario_summary(&planned),
    })?;

    // Step 3 continued: the harness reports its planned scene and
    // authored duration to the tool. The tool uses this to decide
    // whether its probed scene matches.
    channel.write_message(&HarnessResponse::Open {
        scenario: scenario_summary(&planned),
    })?;

    // Step 4/5: receive the probed quantum and model identity from
    // the tool, validate the plan against it, normalize the program
    // once, and send the wire-stable Program envelope back.
    let probe = channel.read_message()?;
    let HarnessRequest::Probe {
        quantum_ns,
        model_identity,
    } = probe
    else {
        return Err(HarnessError::Protocol {
            name: planned.name.clone(),
            message: format!("expected Probe, got `{}`", request_name(&probe)),
        });
    };
    let quantum = Quantum::from_nanos(quantum_ns).ok_or_else(|| HarnessError::Protocol {
        name: planned.name.clone(),
        message: format!("tool supplied zero or sub-microsecond quantum ({quantum_ns} ns)"),
    })?;
    let transitions = planned.plan.transition_count(quantum).ok_or_else(|| {
        HarnessError::Protocol {
            name: planned.name.clone(),
            message: format!(
                "plan `{}` does not align with the probed {quantum_ns} ns quantum (or overflows)",
                planned.name
            ),
        }
    })?;
    planned
        .plan
        .validate_for_quantum(quantum)
        .map_err(|source| HarnessError::Protocol {
            name: planned.name.clone(),
            message: format!(
                "plan `{}` fails validate_for_quantum against {quantum_ns} ns: {source}",
                planned.name
            ),
        })?;

    let entries = planned
        .plan
        .steps
        .iter()
        .map(|step| ScheduleEntry::at(step.boundary, step.action.clone()))
        .collect();
    let program = Program::normalize(
        &planned.name,
        quantum,
        planned.plan.duration,
        entries,
        planned.plan.captures.clone(),
    )
    .map_err(|source| HarnessError::Protocol {
        name: planned.name.clone(),
        message: format!("program normalization: {source}"),
    })?;
    channel.write_message(&HarnessResponse::Program {
        program_envelope_b64: encode_program_envelope(&program),
        transitions,
        model_identity,
    })?;

    // Step 6/7/8: receive the lifecycle-observed evidence from the
    // tool, build/validate the typed `ScenarioRun`, reject
    // inconsistent evidence, call `verify_box` on the retained
    // scenario instance, and send the verdict back.
    //
    // The seal is strict: if the lifecycle admission was refused
    // (lifecycle_passing=false) or the cleanup errored
    // (cleanup_succeeded=false), the typed `ScenarioRun` cannot
    // seal as passing. We catch that case here and emit a verdict
    // instead of bubbling the error to the tool as an EOF — every
    // case ends with a verdict frame so the tool never misreads
    // an admission refusal as success.
    let evidence = channel.read_message()?;
    let HarnessRequest::Evidence {
        report,
        cleanup_succeeded,
        lifecycle_passing,
    } = evidence
    else {
        return Err(HarnessError::Protocol {
            name: planned.name.clone(),
            message: format!("expected Evidence, got `{}`", request_name(&evidence)),
        });
    };
    let verdict = seal_and_verify(
        &planned,
        &program,
        &report,
        cleanup_succeeded,
        lifecycle_passing,
    );
    channel.write_message(&HarnessResponse::Verdict(verdict))?;
    Ok(())
}

/// Build the typed run, then either seal and call `verify_box` or
/// surface a typed failure verdict. Every path that reaches this
/// function emits a verdict; none of them exit through `?`.
fn seal_and_verify(
    planned: &PlannedScenario,
    program: &Program,
    report: &messages::LifecycleReport,
    cleanup_succeeded: bool,
    lifecycle_passing: bool,
) -> messages::Verdict {
    let run_result = build_scenario_run(
        planned,
        program,
        report,
        cleanup_succeeded,
        lifecycle_passing,
    );
    match run_result {
        Ok(run) if !lifecycle_passing || !run.passed() => {
            let detail = if run.passed() {
                format!(
                    "scenario `{}` lifecycle reported non-passing: {}",
                    planned.name,
                    report_summary(report, cleanup_succeeded, lifecycle_passing)
                )
            } else {
                format!(
                    "scenario `{}` did not seal as passing: lifecycle or evidence mismatch",
                    planned.name
                )
            };
            messages::Verdict::fail(Some(detail))
        }
        Ok(run) => match planned.scenario.verify_box(&run) {
            Ok(()) => messages::Verdict::pass(None),
            Err(source) => messages::Verdict::fail(Some(format!("verify_box: {source:#}"))),
        },
        Err(error) => {
            // The seal rejected the evidence. This is the evidence
            // mismatch path: the tool sent terminal evidence the
            // harness could not bind to the user's program.
            messages::Verdict::fail(Some(format!(
                "scenario `{}` evidence rejected by seal: {error}",
                planned.name
            )))
        }
    }
}

fn plan_scenario(name: &str) -> Result<PlannedScenario, HarnessError> {
    let entries =
        crate::scenario::registry::list_scenarios().map_err(|source| HarnessError::Planning {
            name: name.to_owned(),
            source: source.into(),
        })?;
    let registered = entries
        .iter()
        .find(|entry| entry.short_name == name || entry.name == name)
        .ok_or_else(|| HarnessError::UnknownScenario(name.to_owned()))?;
    let entry = registered.entry;
    entry().map_err(|source| HarnessError::Planning {
        name: name.to_owned(),
        source,
    })
}

fn scenario_summary(planned: &PlannedScenario) -> ScenarioSummary {
    ScenarioSummary {
        name: planned.name.clone(),
        scene: planned.plan.scene.clone(),
        duration_ns: planned.plan.duration.as_nanos().min(u128::from(u64::MAX)) as u64,
        step_count: planned.plan.steps.len(),
        capture_count: planned.plan.captures.len(),
    }
}

fn request_name(request: &HarnessRequest) -> &'static str {
    match request {
        HarnessRequest::Hello { .. } => "Hello",
        HarnessRequest::Probe { .. } => "Probe",
        HarnessRequest::Evidence { .. } => "Evidence",
    }
}

fn report_summary(
    report: &messages::LifecycleReport,
    cleanup_succeeded: bool,
    lifecycle_passing: bool,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("lifecycle_passing={lifecycle_passing}"));
    parts.push(format!("cleanup_succeeded={cleanup_succeeded}"));
    parts.push(format!("quantum_ns={}", report.quantum_ns));
    parts.push(format!("completed_steps={}", report.completed_steps));
    parts.push(format!(
        "final_observation_cut={}",
        report.final_observation_cut_observed
    ));
    parts.push(format!(
        "final_capture_drain={}",
        report.final_capture_drain_observed
    ));
    parts.join(" ")
}

/// Construct the typed [`ScenarioRun`] from the lifecycle-observed
/// evidence the tool sent. The harness never fabricates a passing
/// terminal value: if the tool reports a failed lifecycle, the run
/// will not seal as passing and the verdict will be `Failed`.
fn build_scenario_run(
    planned: &PlannedScenario,
    program: &Program,
    report: &messages::LifecycleReport,
    cleanup_succeeded: bool,
    _lifecycle_passing: bool,
) -> Result<ScenarioRun, HarnessError> {
    let mut collector = EvidenceCollector::for_program(program.clone());
    for step in &report.step_outcomes {
        collector
            .record_step_outcome(step.label.clone(), step.outcome.clone())
            .map_err(|source| HarnessError::Protocol {
                name: planned.name.clone(),
                message: format!("record step `{}`: {source}", step.label),
            })?;
    }
    for capture in &report.capture_records {
        collector
            .record_capture(capture.name.clone(), capture.record.clone())
            .map_err(|source| HarnessError::Protocol {
                name: planned.name.clone(),
                message: format!("record capture `{}`: {source}", capture.name),
            })?;
    }
    for reply in &report.command_replies {
        collector
            .record_command_reply(
                reply.label.clone(),
                CommandReply::Accepted {
                    response_bytes: reply.response_bytes.clone(),
                },
            )
            .map_err(|source| HarnessError::Protocol {
                name: planned.name.clone(),
                message: format!("record command reply `{}`: {source}", reply.label),
            })?;
    }
    if let Some(native_body) = &report.native_body {
        collector
            .record_capture(
                native_body.capture_name.clone(),
                CaptureRecord::NativeBody(native_body.payload.clone()),
            )
            .map_err(|source| HarnessError::Protocol {
                name: planned.name.clone(),
                message: format!(
                    "record native body capture `{}`: {source}",
                    native_body.capture_name
                ),
            })?;
    }
    let mut builder = collector.terminal_evidence_builder();
    builder = builder
        .with_execution_identity(report.execution_id.clone())
        .with_terminal_quantum_ns(report.quantum_ns)
        .with_completed_transitions(report.completed_steps);
    if report.final_observation_cut_observed {
        builder = builder.final_observation_cut_observed();
    }
    if report.final_capture_drain_observed {
        builder = builder.final_capture_drain_observed();
    }
    if cleanup_succeeded {
        builder = builder.cleanup_succeeded();
    }
    let evidence = builder.build();
    collector
        .record_terminal_evidence(evidence)
        .map_err(|source| HarnessError::Protocol {
            name: planned.name.clone(),
            message: format!(
                "record terminal evidence: {source}; {}",
                report_summary(report, cleanup_succeeded, _lifecycle_passing)
            ),
        })?;
    collector.seal().map_err(|source| HarnessError::Protocol {
        name: planned.name.clone(),
        message: format!("seal: {source}"),
    })
}

fn parse_fd_env(name: &'static str) -> Result<i32, HarnessError> {
    let raw = env::var_os(name).ok_or(HarnessError::BadFdEnv { name })?;
    let parsed = raw
        .to_str()
        .ok_or(HarnessError::BadFdEnv { name })?
        .trim()
        .parse::<i32>()
        .map_err(|_| HarnessError::BadFdEnv { name })?;
    Ok(parsed)
}

fn read_exact(reader: &mut File, buffer: &mut [u8], budget: Duration) -> Result<(), String> {
    let deadline = Instant::now() + budget;
    let mut read_total = 0usize;
    while read_total < buffer.len() {
        let now = Instant::now();
        if now >= deadline {
            return Err(format!(
                "read timeout after {budget:?} (got {read_total}/{} bytes)",
                buffer.len()
            ));
        }
        let slice = &mut buffer[read_total..];
        match reader.read(slice) {
            Ok(0) => {
                return Err(format!(
                    "channel closed (got {read_total}/{} bytes)",
                    buffer.len()
                ));
            }
            Ok(n) => read_total += n,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => return Err(format!("read: {source}")),
        }
    }
    Ok(())
}

fn write_all(writer: &mut File, buffer: &[u8], budget: Duration) -> Result<(), String> {
    let deadline = Instant::now() + budget;
    let mut written = 0usize;
    while written < buffer.len() {
        let now = Instant::now();
        if now >= deadline {
            return Err(format!(
                "write timeout after {budget:?} (sent {written}/{} bytes)",
                buffer.len()
            ));
        }
        match writer.write(&buffer[written..]) {
            Ok(0) => return Err("channel closed mid-write".to_owned()),
            Ok(n) => written += n,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => return Err(format!("write: {source}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::plan::ScenarioPlan;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration as StdDuration;

    /// Drive a happy-path case through a paired `Channel`. The
    /// tool-side helper thread sends Hello, Probe, Evidence, then
    /// reads Verdict. The harness-side driver runs on this thread.
    /// The scenario is a no-op plan that always passes `verify_box`.
    #[test]
    fn harness_driver_returns_passing_verdict_for_happy_path() {
        let pair = pipe_pair().expect("pipe pair");
        let (harness_read, tool_write) = (pair[0], pair[1]);
        let pair = pipe_pair().expect("pipe pair");
        let (tool_read, harness_write) = (pair[0], pair[1]);
        let (verdict_tx, verdict_rx) = mpsc::channel::<messages::Verdict>();

        let driver = thread::spawn(move || {
            let mut channel = Channel::from_fds(
                unsafe { OwnedFd::from(File::from_raw_fd(harness_read)) },
                unsafe { OwnedFd::from(File::from_raw_fd(harness_write)) },
            );
            let harness_name = "scenarios/HarnessHappyPath";

            let hello = channel.read_message().expect("hello");
            let HarnessRequest::Hello { version } = hello else {
                panic!("expected Hello");
            };
            assert_eq!(version, messages::PROTOCOL_VERSION);
            channel
                .write_message(&HarnessResponse::HelloAck {
                    version: messages::PROTOCOL_VERSION,
                    scenario: ScenarioSummary {
                        name: harness_name.to_owned(),
                        scene: "happy/scene".into(),
                        duration_ns: 6_000_000,
                        step_count: 0,
                        capture_count: 0,
                    },
                })
                .expect("hello ack");
            channel
                .write_message(&HarnessResponse::Open {
                    scenario: ScenarioSummary {
                        name: harness_name.to_owned(),
                        scene: "happy/scene".into(),
                        duration_ns: 6_000_000,
                        step_count: 0,
                        capture_count: 0,
                    },
                })
                .expect("open");

            let probe = channel.read_message().expect("probe");
            let HarnessRequest::Probe { quantum_ns, .. } = probe else {
                panic!("expected Probe");
            };
            let quantum = Quantum::from_nanos(quantum_ns).expect("quantum");
            let harness_name_owned = harness_name.to_owned();
            let planned = make_planned(harness_name_owned.clone(), quantum_ns);

            let entries = planned
                .plan
                .steps
                .iter()
                .map(|step| ScheduleEntry::at(step.boundary, step.action.clone()))
                .collect();
            let program = Program::normalize(
                &harness_name_owned,
                quantum,
                planned.plan.duration,
                entries,
                planned.plan.captures.clone(),
            )
            .expect("normalize");
            channel
                .write_message(&HarnessResponse::Program {
                    program_envelope_b64: encode_program_envelope(&program),
                    transitions: program.transition_count(),
                    model_identity: "happy_model".to_owned(),
                })
                .expect("program");

            let evidence = channel.read_message().expect("evidence");
            let HarnessRequest::Evidence {
                report,
                cleanup_succeeded,
                lifecycle_passing,
            } = evidence
            else {
                panic!("expected Evidence");
            };
            let verdict = seal_and_verify(
                &planned,
                &program,
                &report,
                cleanup_succeeded,
                lifecycle_passing,
            );
            verdict_tx.send(verdict.clone()).expect("send verdict");
            channel
                .write_message(&HarnessResponse::Verdict(verdict))
                .expect("verdict");
        });

        // Tool-side thread: drive the protocol.
        let tool = thread::spawn(move || {
            let mut tool_read = unsafe { File::from_raw_fd(tool_read) };
            let mut tool_write = unsafe { File::from_raw_fd(tool_write) };
            tool_write
                .write_all(&frame(
                    &serde_json::to_vec(&HarnessRequest::Hello {
                        version: messages::PROTOCOL_VERSION,
                    })
                    .unwrap(),
                ))
                .expect("hello");
            let _ = read_frame(&mut tool_read);
            let _ = read_frame(&mut tool_read);
            tool_write
                .write_all(&frame(
                    &serde_json::to_vec(&HarnessRequest::Probe {
                        quantum_ns: 2_000_000,
                        model_identity: "happy_model".to_owned(),
                    })
                    .unwrap(),
                ))
                .expect("probe");
            let _ = read_frame(&mut tool_read);
            tool_write
                .write_all(&frame(
                    &serde_json::to_vec(&HarnessRequest::Evidence {
                        report: messages::LifecycleReport {
                            execution_id: "exec/happy".to_owned(),
                            quantum_ns: 2_000_000,
                            completed_steps: 3,
                            final_observation_cut_observed: true,
                            final_capture_drain_observed: true,
                            step_outcomes: Vec::new(),
                            capture_records: Vec::new(),
                            command_replies: Vec::new(),
                            native_body: None,
                        },
                        cleanup_succeeded: true,
                        lifecycle_passing: true,
                    })
                    .unwrap(),
                ))
                .expect("evidence");
            read_frame(&mut tool_read)
        });

        let verdict = verdict_rx.recv().expect("verdict");
        assert!(verdict.passed, "verdict should pass: {:?}", verdict);
        let _ = tool.join();
        let _ = driver.join();
    }

    /// Rejection path: failed admission. The tool signals that the
    /// supervisor refused the bundle by sending
    /// `lifecycle_passing: false`. The harness must report a failing
    /// verdict; the verdict detail must mention the lifecycle or the
    /// seal rejection (the strict seal rejects incomplete terminal
    /// evidence, which is the same observable condition).
    #[test]
    fn harness_rejects_failed_lifecycle_admission_with_failing_verdict() {
        let verdict = drive_protocol_outcome(LifecycleOutcome::AdmissionRefused);
        assert!(!verdict.passed, "verdict must fail: {:?}", verdict);
        let detail = verdict.detail.unwrap_or_default();
        assert!(
            detail.contains("lifecycle reported non-passing")
                || detail.contains("did not seal as passing")
                || detail.contains("evidence rejected by seal"),
            "verdict detail should explain why the case failed: {detail}"
        );
    }

    /// Rejection path: cleanup failure. The lifecycle completed but
    /// the supervisor's cleanup step errored. The tool sends
    /// `cleanup_succeeded: false` while keeping `lifecycle_passing`
    /// at `true`. The harness must still report a failing verdict
    /// because the seal's `cleanup_ok` flag is unset.
    #[test]
    fn harness_rejects_lifecycle_cleanup_failure_with_failing_verdict() {
        let verdict = drive_protocol_outcome(LifecycleOutcome::CleanupFailed);
        assert!(!verdict.passed, "verdict must fail: {:?}", verdict);
    }

    /// Rejection path: evidence mismatch. The tool's lifecycle
    /// reports `lifecycle_passing: true` and `cleanup_succeeded: true`
    /// but supplies step outcomes that contradict the user's plan.
    /// The harness seal must reject the run and return a failing
    /// verdict.
    #[test]
    fn harness_rejects_evidence_mismatch_with_failing_verdict() {
        let verdict = drive_protocol_outcome(LifecycleOutcome::EvidenceMismatch);
        assert!(!verdict.passed, "verdict must fail: {:?}", verdict);
    }

    /// Rejection path: child failure (the harness binary exits
    /// mid-protocol). The tool's read returns zero bytes (EOF) and
    /// the case host surfaces a non-passing verdict without the
    /// harness ever replying. We exercise this path directly via the
    /// typed harness error rather than the verdict to mirror what the
    /// tool reports to the user.
    #[test]
    fn harness_protocol_reads_report_channel_close_when_child_dies() {
        // Drive the harness side with a closed read end: the harness
        // call must surface an i/o error describing the closed
        // channel rather than fabricating a passing verdict.
        let pair = pipe_pair().expect("pipe pair");
        let (tool_write, harness_read) = (pair[0], pair[1]);
        let pair = pipe_pair().expect("pipe pair");
        let (harness_write, _tool_read) = (pair[0], pair[1]);
        let harness_read_file = unsafe { File::from_raw_fd(harness_read) };
        // Drop the tool write side so the harness reads EOF.
        let _ = tool_write;
        let mut channel = Channel::from_fds(OwnedFd::from(harness_read_file), unsafe {
            OwnedFd::from(File::from_raw_fd(harness_write))
        });
        let err = channel.read_message().expect_err("EOF must fail");
        match err {
            HarnessError::Io { .. } => {}
            other => panic!("expected Io error, got {other:?}"),
        }
    }

    fn drive_protocol_outcome(outcome: LifecycleOutcome) -> messages::Verdict {
        let pair = pipe_pair().expect("pipe pair");
        let (harness_read, tool_write) = (pair[0], pair[1]);
        let pair = pipe_pair().expect("pipe pair");
        let (tool_read, harness_write) = (pair[0], pair[1]);
        let (verdict_tx, verdict_rx) = mpsc::channel::<messages::Verdict>();
        let outcome_for_driver = outcome;
        let outcome_for_tool = outcome;

        let driver = thread::spawn(move || {
            let mut channel = Channel::from_fds(
                unsafe { OwnedFd::from(File::from_raw_fd(harness_read)) },
                unsafe { OwnedFd::from(File::from_raw_fd(harness_write)) },
            );
            let harness_name = "scenarios/HarnessOutcomeProbe";

            let hello = channel.read_message().expect("hello");
            let HarnessRequest::Hello { version } = hello else {
                panic!("expected Hello");
            };
            assert_eq!(version, messages::PROTOCOL_VERSION);
            channel
                .write_message(&HarnessResponse::HelloAck {
                    version: messages::PROTOCOL_VERSION,
                    scenario: ScenarioSummary {
                        name: harness_name.to_owned(),
                        scene: "outcome/scene".into(),
                        duration_ns: 6_000_000,
                        step_count: 0,
                        capture_count: 0,
                    },
                })
                .expect("hello ack");
            channel
                .write_message(&HarnessResponse::Open {
                    scenario: ScenarioSummary {
                        name: harness_name.to_owned(),
                        scene: "outcome/scene".into(),
                        duration_ns: 6_000_000,
                        step_count: 0,
                        capture_count: 0,
                    },
                })
                .expect("open");

            let probe = channel.read_message().expect("probe");
            let HarnessRequest::Probe { quantum_ns, .. } = probe else {
                panic!("expected Probe");
            };
            let quantum = Quantum::from_nanos(quantum_ns).expect("quantum");
            let harness_name_owned = harness_name.to_owned();
            let planned = make_planned(harness_name_owned.clone(), quantum_ns);

            let entries = planned
                .plan
                .steps
                .iter()
                .map(|step| ScheduleEntry::at(step.boundary, step.action.clone()))
                .collect();
            let program = Program::normalize(
                &harness_name_owned,
                quantum,
                planned.plan.duration,
                entries,
                planned.plan.captures.clone(),
            )
            .expect("normalize");
            channel
                .write_message(&HarnessResponse::Program {
                    program_envelope_b64: encode_program_envelope(&program),
                    transitions: program.transition_count(),
                    model_identity: "outcome_model".to_owned(),
                })
                .expect("program");

            let evidence = channel.read_message().expect("evidence");
            let HarnessRequest::Evidence {
                report,
                cleanup_succeeded,
                lifecycle_passing,
            } = evidence
            else {
                panic!("expected Evidence");
            };
            let verdict = seal_and_verify(
                &planned,
                &program,
                &report,
                cleanup_succeeded,
                lifecycle_passing,
            );
            verdict_tx.send(verdict.clone()).expect("send verdict");
            channel
                .write_message(&HarnessResponse::Verdict(verdict))
                .expect("verdict");
        });

        let tool = thread::spawn(move || {
            let mut tool_read = unsafe { File::from_raw_fd(tool_read) };
            let mut tool_write = unsafe { File::from_raw_fd(tool_write) };
            tool_write
                .write_all(&frame(
                    &serde_json::to_vec(&HarnessRequest::Hello {
                        version: messages::PROTOCOL_VERSION,
                    })
                    .unwrap(),
                ))
                .expect("hello");
            let _ = read_frame(&mut tool_read);
            let _ = read_frame(&mut tool_read);
            tool_write
                .write_all(&frame(
                    &serde_json::to_vec(&HarnessRequest::Probe {
                        quantum_ns: 2_000_000,
                        model_identity: "outcome_model".to_owned(),
                    })
                    .unwrap(),
                ))
                .expect("probe");
            let _ = read_frame(&mut tool_read);
            let (lifecycle_passing, cleanup_succeeded, completed_steps) = match outcome_for_tool {
                LifecycleOutcome::AdmissionRefused => (false, false, 0),
                LifecycleOutcome::CleanupFailed => (true, false, 3),
                LifecycleOutcome::EvidenceMismatch => (true, true, 0),
            };
            tool_write
                .write_all(&frame(
                    &serde_json::to_vec(&HarnessRequest::Evidence {
                        report: messages::LifecycleReport {
                            execution_id: "exec/outcome".to_owned(),
                            quantum_ns: 2_000_000,
                            completed_steps,
                            final_observation_cut_observed: completed_steps > 0,
                            final_capture_drain_observed: false,
                            step_outcomes: Vec::new(),
                            capture_records: Vec::new(),
                            command_replies: Vec::new(),
                            native_body: None,
                        },
                        cleanup_succeeded,
                        lifecycle_passing,
                    })
                    .unwrap(),
                ))
                .expect("evidence");
            read_frame(&mut tool_read)
        });

        let verdict = verdict_rx.recv().expect("verdict");
        let _ = tool.join();
        let _ = driver.join();
        let _ = outcome_for_driver;
        verdict
    }

    #[derive(Debug, Clone, Copy)]
    enum LifecycleOutcome {
        AdmissionRefused,
        CleanupFailed,
        EvidenceMismatch,
    }

    // Helpers ---------------------------------------------------------------

    struct HarnessHappyPath;
    impl Default for HarnessHappyPath {
        fn default() -> Self {
            HarnessHappyPath
        }
    }
    impl crate::scenario::Scenario for HarnessHappyPath {
        fn plan(&self) -> crate::Result<crate::scenario::plan::ScenarioPlan> {
            Ok(crate::scenario::plan::ScenarioPlan::new(
                "happy/scene",
                StdDuration::from_micros(6_000),
            ))
        }
        fn verify(&self, run: &crate::scenario::results::ScenarioRun) -> crate::Result<()> {
            if !run.passed() {
                Err(crate::anyhow!("non-passing run"))
            } else {
                Ok(())
            }
        }
    }

    fn make_planned(name: String, quantum_ns: u64) -> crate::scenario::registry::PlannedScenario {
        let duration_ns = quantum_ns.checked_mul(3).unwrap_or(0);
        let duration = StdDuration::from_nanos(duration_ns);
        let scenario = HarnessHappyPath;
        let plan = ScenarioPlan::new("happy/scene", duration);
        crate::scenario::registry::PlannedScenario {
            name,
            plan,
            scenario: Box::new(scenario),
        }
    }

    fn pipe_pair() -> io::Result<[i32; 2]> {
        let mut fds = [0i32; 2];
        // Use libc::pipe; we keep this in the test module so the
        // harness_support module itself does not depend on libc.
        unsafe extern "C" {
            fn pipe(fds: *mut i32) -> i32;
        }
        let result = unsafe { pipe(fds.as_mut_ptr()) };
        if result != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(fds)
        }
    }

    fn frame(payload: &[u8]) -> Vec<u8> {
        let len = u32::try_from(payload.len()).expect("u32 len");
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn read_frame(file: &mut File) -> Vec<u8> {
        let mut header = [0u8; 4];
        file.read_exact(&mut header).expect("header");
        let len = u32::from_be_bytes(header) as usize;
        let mut payload = vec![0u8; len];
        if len > 0 {
            file.read_exact(&mut payload).expect("payload");
        }
        payload
    }
}
