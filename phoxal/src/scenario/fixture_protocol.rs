//! Invocation-scoped local protocol between simulation tests and
//! `cargo phoxal test`.

#![allow(missing_docs)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{
    CaptureRecord, CommandReply, EvidenceCollector, Program, Quantum, ScenarioRun, StepOutcome,
};
use crate::scenario::fixture::{CompletedRun, Plan};

pub const PROTOCOL_VERSION: u32 = 2;
pub const ENV_ENDPOINT: &str = "PHOXAL_TEST_FIXTURE_ENDPOINT";
pub const ENV_PROJECT_ROOT: &str = "PHOXAL_TEST_PROJECT_ROOT";
pub const ENV_SCENE: &str = "PHOXAL_TEST_SCENE";
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    Open {
        version: u32,
        request_id: String,
        test_identity: String,
        scene: PathBuf,
    },
    Execute {
        request_id: String,
        program: Vec<u8>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostMessage {
    Probe {
        version: u32,
        request_id: String,
        quantum_ns: u64,
        model_identity: String,
    },
    Completed {
        request_id: String,
        report: LifecycleReport,
        cleanup_succeeded: bool,
        lifecycle_passing: bool,
    },
    Failed {
        request_id: String,
        failure: RunFailure,
    },
}

/// Structured infrastructure failure returned by the run host.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunFailure {
    pub phase: String,
    pub cause: String,
    pub cleanup: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<LifecycleReport>,
}

/// Finalized lifecycle evidence returned by the run host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleReport {
    pub execution_id: String,
    pub quantum_ns: u64,
    pub completed_steps: u64,
    pub final_observation_cut_observed: bool,
    pub final_capture_drain_observed: bool,
    #[serde(default)]
    pub step_outcomes: Vec<StepOutcomeRecord>,
    #[serde(default)]
    pub capture_records: Vec<CaptureRecordRef>,
    #[serde(default)]
    pub command_replies: Vec<CommandReplyRef>,
    #[serde(default)]
    pub native_body: Option<NativeBodyRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepOutcomeRecord {
    pub label: String,
    pub outcome: StepOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRecordRef {
    pub name: String,
    pub record: CaptureRecord,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandReplyRef {
    pub label: String,
    pub response_bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeBodyRef {
    pub capture_name: String,
    pub payload: Vec<u8>,
}

impl std::fmt::Display for RunFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "simulation run failed during {}: {} (cleanup: {})",
            self.phase, self.cause, self.cleanup
        )
    }
}

impl std::error::Error for RunFailure {}

pub(crate) fn run(test_identity: &str, scene: &Path, plan: Plan) -> crate::Result<CompletedRun> {
    let endpoint = std::env::var_os(ENV_ENDPOINT).ok_or_else(|| {
        crate::anyhow!(
            "simulation fixture has no command-scoped run host; execute this test with `cargo phoxal test`"
        )
    })?;
    let endpoint = PathBuf::from(endpoint);
    #[cfg(not(unix))]
    {
        let _ = (test_identity, scene, plan, endpoint);
        return Err(crate::anyhow!(
            "simulation fixture execution is currently supported only on Unix hosts"
        ));
    }
    #[cfg(unix)]
    {
        let mut stream = std::os::unix::net::UnixStream::connect(&endpoint).map_err(|error| {
            crate::anyhow!(
                "cannot connect to cargo phoxal test run host at {}: {error}",
                endpoint.display()
            )
        })?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let request_id = format!("{}-{}", std::process::id(), plan.id);
        write_message(
            &mut stream,
            &ClientMessage::Open {
                version: PROTOCOL_VERSION,
                request_id: request_id.clone(),
                test_identity: test_identity.to_owned(),
                scene: scene.to_owned(),
            },
        )?;
        let probe: HostMessage = read_message(&mut stream)?;
        let (quantum_ns, _model_identity) = match probe {
            HostMessage::Probe {
                version,
                request_id: returned,
                quantum_ns,
                model_identity,
            } => {
                if version != PROTOCOL_VERSION {
                    return Err(crate::anyhow!(
                        "fixture protocol version mismatch: client {}, host {version}",
                        PROTOCOL_VERSION
                    ));
                }
                check_request(&request_id, &returned)?;
                (quantum_ns, model_identity)
            }
            HostMessage::Failed {
                request_id: returned,
                failure,
            } => {
                check_request(&request_id, &returned)?;
                return Err(failure.into());
            }
            HostMessage::Completed { .. } => {
                return Err(crate::anyhow!(
                    "run host completed before receiving a program"
                ));
            }
        };
        let quantum = Quantum::from_nanos(quantum_ns).ok_or_else(|| {
            crate::anyhow!("run host supplied unsupported quantum {quantum_ns} ns")
        })?;
        let (program, plan_id) = plan.compile(scene.to_owned(), quantum, test_identity)?;
        write_message(
            &mut stream,
            &ClientMessage::Execute {
                request_id: request_id.clone(),
                program: program.program_bytes().to_vec(),
            },
        )?;
        let completed: HostMessage = read_message(&mut stream)?;
        match completed {
            HostMessage::Completed {
                request_id: returned,
                report,
                cleanup_succeeded,
                lifecycle_passing,
            } => {
                check_request(&request_id, &returned)?;
                if !lifecycle_passing {
                    return Err(crate::anyhow!(
                        "simulation lifecycle did not complete successfully"
                    ));
                }
                let run = build_run(test_identity, &program, &report, cleanup_succeeded)?;
                Ok(CompletedRun::new(plan_id, run))
            }
            HostMessage::Failed {
                request_id: returned,
                failure,
            } => {
                check_request(&request_id, &returned)?;
                Err(failure.into())
            }
            HostMessage::Probe { .. } => {
                Err(crate::anyhow!("run host sent a duplicate probe response"))
            }
        }
    }
}

fn check_request(expected: &str, actual: &str) -> crate::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(crate::anyhow!(
            "fixture response isolation failure: expected request `{expected}`, got `{actual}`"
        ))
    }
}

fn build_run(
    name: &str,
    program: &Program,
    report: &LifecycleReport,
    cleanup_succeeded: bool,
) -> crate::Result<ScenarioRun> {
    let mut collector = EvidenceCollector::for_program(program.clone());
    for step in &report.step_outcomes {
        collector
            .record_step_outcome(step.label.clone(), step.outcome.clone())
            .map_err(|error| crate::anyhow!("{name}: step evidence rejected: {error}"))?;
    }
    for capture in &report.capture_records {
        collector
            .record_capture(capture.name.clone(), capture.record.clone())
            .map_err(|error| crate::anyhow!("{name}: capture evidence rejected: {error}"))?;
    }
    for reply in &report.command_replies {
        collector
            .record_command_reply(
                reply.label.clone(),
                CommandReply::Accepted {
                    response_bytes: reply.response_bytes.clone(),
                },
            )
            .map_err(|error| crate::anyhow!("{name}: reply evidence rejected: {error}"))?;
    }
    if let Some(body) = &report.native_body {
        collector
            .record_capture(
                body.capture_name.clone(),
                CaptureRecord::NativeBody(body.payload.clone()),
            )
            .map_err(|error| crate::anyhow!("{name}: native evidence rejected: {error}"))?;
    }
    let mut terminal = collector
        .terminal_evidence_builder()
        .with_execution_identity(report.execution_id.clone())
        .with_terminal_quantum_ns(report.quantum_ns)
        .with_completed_transitions(report.completed_steps);
    if report.final_observation_cut_observed {
        terminal = terminal.final_observation_cut_observed();
    }
    if report.final_capture_drain_observed {
        terminal = terminal.final_capture_drain_observed();
    }
    if cleanup_succeeded {
        terminal = terminal.cleanup_succeeded();
    }
    collector
        .record_terminal_evidence(terminal.build())
        .map_err(|error| crate::anyhow!("{name}: terminal evidence rejected: {error}"))?;
    collector
        .seal()
        .map_err(|error| crate::anyhow!("{name}: completed evidence did not seal: {error}"))
}

pub fn write_message<T: Serialize>(writer: &mut impl Write, value: &T) -> crate::Result<()> {
    let payload = serde_json::to_vec(value)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(crate::anyhow!(
            "fixture protocol frame is {} bytes; cap is {MAX_FRAME_BYTES}",
            payload.len()
        ));
    }
    let length = u32::try_from(payload.len())?.to_be_bytes();
    writer.write_all(&length)?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

pub fn read_message<T: for<'de> Deserialize<'de>>(reader: &mut impl Read) -> crate::Result<T> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(crate::anyhow!(
            "fixture protocol frame declares {length} bytes; cap is {MAX_FRAME_BYTES}"
        ));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn framed_messages_round_trip_with_their_request_identity() {
        let message = ClientMessage::Open {
            version: PROTOCOL_VERSION,
            request_id: "package-a::same-test/1".to_owned(),
            test_identity: "package-a::same-test".to_owned(),
            scene: PathBuf::from("simulation/scene.xml"),
        };
        let mut bytes = Vec::new();
        write_message(&mut bytes, &message).expect("write frame");
        let decoded: ClientMessage = read_message(&mut Cursor::new(bytes)).expect("read frame");
        let ClientMessage::Open {
            request_id,
            test_identity,
            ..
        } = decoded
        else {
            panic!("wrong message kind")
        };
        assert_eq!(request_id, "package-a::same-test/1");
        assert_eq!(test_identity, "package-a::same-test");
    }

    #[test]
    fn oversized_and_disconnected_frames_fail_without_a_partial_result() {
        let mut oversized = Cursor::new(((MAX_FRAME_BYTES as u32) + 1).to_be_bytes().to_vec());
        assert!(read_message::<ClientMessage>(&mut oversized).is_err());

        let mut disconnected = Cursor::new(16_u32.to_be_bytes().to_vec());
        assert!(read_message::<ClientMessage>(&mut disconnected).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn simultaneous_clients_with_repeated_test_names_keep_results_isolated() {
        use std::os::unix::net::UnixStream;
        use std::sync::{Arc, Barrier};

        let (mut first_client, mut first_host) = UnixStream::pair().expect("first socket pair");
        let (mut second_client, mut second_host) = UnixStream::pair().expect("second socket pair");
        let barrier = Arc::new(Barrier::new(3));
        std::thread::scope(|scope| {
            for (mut client, package, request_id) in [
                (&mut first_client, "package-a", "request-a"),
                (&mut second_client, "package-b", "request-b"),
            ] {
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    write_message(
                        &mut client,
                        &ClientMessage::Open {
                            version: PROTOCOL_VERSION,
                            request_id: request_id.to_owned(),
                            test_identity: format!("{package}::same_test"),
                            scene: PathBuf::from("simulation/scene.xml"),
                        },
                    )
                    .expect("write open");
                    let response: HostMessage = read_message(&mut client).expect("read response");
                    let HostMessage::Probe {
                        request_id: returned,
                        model_identity,
                        ..
                    } = response
                    else {
                        panic!("wrong response kind")
                    };
                    assert_eq!(returned, request_id);
                    assert_eq!(model_identity, package);
                });
            }
            for (mut host, package) in [
                (&mut first_host, "package-a"),
                (&mut second_host, "package-b"),
            ] {
                scope.spawn(move || {
                    let open: ClientMessage = read_message(&mut host).expect("read open");
                    let ClientMessage::Open { request_id, .. } = open else {
                        panic!("wrong request kind")
                    };
                    write_message(
                        &mut host,
                        &HostMessage::Probe {
                            version: PROTOCOL_VERSION,
                            request_id,
                            quantum_ns: 2_000_000,
                            model_identity: package.to_owned(),
                        },
                    )
                    .expect("write response");
                });
            }
            barrier.wait();
        });
    }

    #[test]
    fn result_request_ids_cannot_cross_plan_boundaries() {
        let error = check_request("request-a", "request-b").expect_err("crossed response");
        assert!(error.to_string().contains("response isolation failure"));
    }
}
