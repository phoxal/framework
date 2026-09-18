//! Immutable program representation. A `Program` is the on-disk
//! artifact that pairs a scenario with one finite quantized schedule.
//!
//! The public identity surface is the canonical bytes (`program_bytes`)
//! and the SHA-256 digest computed over those exact bytes. Identity
//! checks compare the digest against the *stored* bytes — never against
//! a re-serialization of the decoded typed fields. The decoded fields
//! are exposed for verifier consumption but cannot be used to forge a
//! different identity after the program is sealed.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_ENGINE;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::scenario::plan::{Action, Capture, MAX_PAYLOAD, Step, Validity};

/// One immutable, serializable scenario program. Constructed via
/// [`Program::normalize`], which is the single validation owner for the
/// program surface: it derives `transition_count` from the validated
/// quantum and duration, deterministically orders steps, records the
/// exact byte length and SHA-256 digest of the canonical wire form,
/// and refuses malformed inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    schema_version: u32,
    scenario_name: String,
    /// Resolved native quantum. A duration of six seconds at a two ms
    /// quantum is a 3000-transition schedule regardless of how many
    /// actions the author wrote into it.
    quantum: Quantum,
    /// Total transition count derived from the validated duration and
    /// quantum at construction. Authoritative for all runtime checks.
    transition_count: u32,
    steps: Vec<Step>,
    captures: Vec<Capture>,
    program_bytes: Vec<u8>,
    program_digest: String,
    byte_length: u32,
}

/// Errors returned by [`Program::normalize`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramError {
    EmptyScenarioName,
    DuplicateStepLabel(String),
    /// The plan's quantum indices exceed the schedule's transition count
    /// derived from the validated quantum and duration.
    QuantumOutOfRange {
        label: String,
        quantum_index: u32,
        transitions: u32,
    },
    PayloadTooLarge {
        step_label: String,
        bytes: usize,
    },
    EmptyPayload {
        step_label: String,
    },
    /// Anything that escaped the plan-level validation surface.
    Other(String),
}

impl fmt::Display for ProgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyScenarioName => write!(f, "scenario name must not be empty"),
            Self::DuplicateStepLabel(label) => {
                write!(f, "program step list contains duplicate label `{label}`")
            }
            Self::QuantumOutOfRange {
                label,
                quantum_index,
                transitions,
            } => write!(
                f,
                "step `{label}` quantum {quantum_index} is at or beyond the final transition {transitions}"
            ),
            Self::PayloadTooLarge { step_label, bytes } => write!(
                f,
                "step `{step_label}` payload is {bytes} bytes, exceeding the {MAX_PAYLOAD}-byte cap"
            ),
            Self::EmptyPayload { step_label } => {
                write!(f, "step `{step_label}` has an empty encoded payload")
            }
            Self::Other(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ProgramError {}

/// The schema version the bundle writes to disk. Bump only on
/// backward-incompatible wire changes; this is the version a reader
/// uses to decide whether to upgrade its decoder.
pub const PROGRAM_SCHEMA_VERSION: u32 = 1;

/// The native quantum of the simulator's discrete tick. The framework
/// resolves a request's duration to a multiple of this quantum and
/// rejects any mismatch. The rover's two millisecond quantum is the
/// canonical default; callers that need a different quantum declare it
/// through `Quantum::from_micros` and carry that exact value through
/// the plan boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quantum(u32);

impl Quantum {
    /// Two milliseconds, in microseconds. The rover's validated quantum.
    pub const DEFAULT_MICROS: u32 = 2_000;

    /// Build a quantum from a positive microsecond value. Zero is
    /// rejected because it would make every non-zero duration map to
    /// infinity transitions.
    pub const fn from_micros(micros: u32) -> Option<Self> {
        if micros == 0 {
            None
        } else {
            Some(Self(micros))
        }
    }

    /// Build a quantum from a positive nanosecond value. Sub-microsecond
    /// quanta are rejected because [`Quantum`] stores microseconds and
    /// the SDK cannot represent finer-grained ticks through the
    /// [`crate::scenario::results::EvidenceCollector`] path. The tool's
    /// harness-support protocol uses this constructor to bridge the
    /// simulator-probed `quantum_ns` to the SDK-side [`Quantum`].
    pub fn from_nanos(nanos: u64) -> Option<Self> {
        if nanos == 0 || !nanos.is_multiple_of(1_000) {
            return None;
        }
        let micros = u32::try_from(nanos / 1_000).ok()?;
        Self::from_micros(micros)
    }

    /// The quantum as a positive microsecond count.
    pub const fn micros(self) -> u32 {
        self.0
    }

    /// The quantum as a positive nanosecond count.
    pub fn nanos(self) -> u64 {
        u64::from(self.0) * 1_000
    }

    /// Resolve a duration to an exact, checked transition count. Returns
    /// `None` only when the duration is an unaligned multiple of the
    /// quantum — every finite duration either aligns or is rejected, so
    /// the caller's "the experiment is six seconds" statement must agree
    /// with the quantum before this function returns a number.
    ///
    /// Uses nanosecond arithmetic so sub-microsecond durations
    /// (e.g. 2 ms plus 1 ns) cannot be silently truncated to a
    /// whole-microsecond multiple that happens to align.
    pub fn transition_count(self, duration: Duration) -> Option<u32> {
        let total_nanos = duration.as_nanos();
        let quantum_nanos = u128::from(self.0) * 1_000;
        if total_nanos == 0 || !total_nanos.is_multiple_of(quantum_nanos) {
            return None;
        }
        let transitions = total_nanos / quantum_nanos;
        u32::try_from(transitions).ok()
    }
}

/// A schedule entry. Separate from `Action` so that an action's
/// authoring order is preserved verbatim at equal boundaries rather
/// than being alphabetised by the program's deterministic-ordering pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleEntry {
    /// The authored boundary, in zero-indexed transitions. Zero is the
    /// first transition, `transition_count - 1` the last.
    pub boundary: u32,
    pub action: Action,
}

impl ScheduleEntry {
    pub fn at(boundary: u32, action: Action) -> Self {
        Self { boundary, action }
    }
}

impl Program {
    /// Construct a normalized program. `scenario_name` is the public
    /// identity (matches `scenarios/<StructIdent>`). The schedule's
    /// transition count is derived from `duration` and the validated
    /// quantum; equal-boundary entries retain their original order in
    /// the wire form so the verifier sees what the author wrote.
    pub fn normalize(
        scenario_name: impl Into<String>,
        quantum: Quantum,
        duration: Duration,
        schedule: Vec<ScheduleEntry>,
        captures: Vec<Capture>,
    ) -> Result<Self, ProgramError> {
        let scenario_name = scenario_name.into();
        if scenario_name.is_empty() {
            return Err(ProgramError::EmptyScenarioName);
        }
        let transitions = quantum.transition_count(duration).ok_or_else(|| {
            ProgramError::Other(format!(
                "duration {} does not align to a {} microsecond quantum",
                duration.as_micros(),
                quantum.micros()
            ))
        })?;
        // Validate labels and quantum bounds before serialization. Order
        // in the wire form follows authored order at equal boundaries
        // (no alphabetical tie-breaker); this keeps the verifier
        // Identical to the author's intent. The schedule is stable-
        // sorted by boundary so [500, 1] becomes [1, 500], and equal
        // boundaries retain the authored order so simultaneous
        // actions remain in the order the author wrote them. Action
        // IDs are assigned by the post-sort ordinal so two
        // simultaneous actions receive distinct identities.
        let mut sorted: Vec<(usize, ScheduleEntry)> = schedule.into_iter().enumerate().collect();
        sorted.sort_by_key(|(_, entry)| entry.boundary);
        // Reject duplicate command labels at the single
        // normalization path. The plan-time validator already does
        // this through ScenarioPlan::with_steps; the fixture/test
        // path that calls Program::normalize directly must apply
        // the same invariant so a tampered or hand-rolled schedule
        // cannot smuggle in two requests with the same correlation
        // label.
        let mut seen_command_labels: BTreeMap<String, usize> = BTreeMap::new();
        for (_, entry) in &sorted {
            if let crate::scenario::plan::Action::Command { label, .. } = &entry.action {
                if let Some(prior_index) = seen_command_labels.get(label) {
                    return Err(ProgramError::Other(format!(
                        "duplicate command label `{label}` at schedule indices {prior_index} and {}",
                        sorted
                            .iter()
                            .position(|(_, candidate)| std::ptr::eq(candidate, entry))
                            .unwrap_or(0)
                    )));
                }
                seen_command_labels.insert(label.clone(), 0);
            }
        }
        let mut steps: Vec<Step> = Vec::with_capacity(sorted.len());
        for (action_index, (_, entry)) in sorted.into_iter().enumerate() {
            if entry.boundary >= transitions {
                return Err(ProgramError::QuantumOutOfRange {
                    label: format!("boundary@{}", entry.boundary),
                    quantum_index: entry.boundary,
                    transitions,
                });
            }
            // Action identity is the post-sort ordinal. This makes
            // simultaneous actions (same boundary, different author
            // position) carry distinct labels, which is what the
            // collector uses to detect duplicate recordings.
            let label = format!("s{:08}", action_index);
            match &entry.action {
                Action::Setpoint {
                    target_instance,
                    encoded_payload,
                    ..
                } => {
                    check_target_instance(&label, target_instance)?;
                    check_payload(&label, encoded_payload)?;
                }
                Action::Command {
                    target_instance,
                    request_encoded,
                    host_deadline,
                    ..
                } => {
                    check_target_instance(&label, target_instance)?;
                    check_payload(&label, request_encoded)?;
                    if *host_deadline == Duration::ZERO {
                        return Err(ProgramError::Other(format!(
                            "step `{label}` declares a zero host deadline"
                        )));
                    }
                }
                Action::Withdraw {
                    target_instance, ..
                } => {
                    check_target_instance(&label, target_instance)?;
                }
            }
            steps.push(Step {
                label,
                boundary: entry.boundary,
                action: entry.action,
            });
        }
        // Canonical wire form: a small JSON envelope. The user keeps
        // writing typed Rust values; the JSON is for storage identity.
        let duration_micros = u64::try_from(duration.as_micros())
            .map_err(|_| ProgramError::Other("duration overflows u64 microseconds".to_owned()))?;
        let envelope = WireProgram {
            schema_version: PROGRAM_SCHEMA_VERSION,
            scenario_name: scenario_name.clone(),
            quantum_micros: quantum.micros(),
            duration_micros,
            steps: steps.iter().map(wire_step).collect(),
            captures: captures.iter().map(wire_capture).collect(),
        };
        let program_bytes = serde_json::to_vec(&envelope)
            .map_err(|error| ProgramError::Other(error.to_string()))?;
        let program_digest = sha256_hex(&program_bytes);
        let byte_length = u32::try_from(program_bytes.len())
            .map_err(|_| ProgramError::Other("program byte length exceeds u32".to_owned()))?;
        Ok(Program {
            schema_version: PROGRAM_SCHEMA_VERSION,
            scenario_name,
            quantum,
            transition_count: transitions,
            steps,
            captures,
            program_bytes,
            program_digest,
            byte_length,
        })
    }

    /// Returns the scenario's public name.
    pub fn scenario_name(&self) -> &str {
        &self.scenario_name
    }

    /// Returns the validated native quantum.
    pub fn quantum(&self) -> Quantum {
        self.quantum
    }

    /// Returns the exact transition count derived from the duration and
    /// quantum at construction. Authoritative for runtime gating.
    pub fn transition_count(&self) -> u32 {
        self.transition_count
    }

    /// Returns the decoded typed steps.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Returns the declared captures.
    pub fn captures(&self) -> &[Capture] {
        &self.captures
    }

    /// Returns the canonical on-disk bytes. The digest was computed
    /// over these exact bytes, so identity checks compare the digest
    /// against `program_bytes` rather than a re-serialization.
    pub fn program_bytes(&self) -> &[u8] {
        &self.program_bytes
    }

    /// Returns the SHA-256 digest of the canonical bytes.
    pub fn program_digest(&self) -> &str {
        &self.program_digest
    }

    /// Returns the canonical byte length.
    pub fn byte_length(&self) -> u32 {
        self.byte_length
    }

    /// Verifies the recorded byte length and digest against the
    /// canonical bytes. Returns `Ok` on a clean identity check, `Err`
    /// if the program was tampered with after recording.
    pub fn verify_identity(&self) -> Result<(), ProgramError> {
        if self.program_bytes.len() as u32 != self.byte_length {
            return Err(ProgramError::Other(format!(
                "byte length mismatch: stored {} actual {}",
                self.byte_length,
                self.program_bytes.len()
            )));
        }
        let actual = sha256_hex(&self.program_bytes);
        if actual != self.program_digest {
            return Err(ProgramError::Other(format!(
                "digest mismatch: stored {} actual {actual}",
                self.program_digest
            )));
        }
        Ok(())
    }

    /// Persists the canonical bytes to disk at `path`. Writes go
    /// through the artifact publication helper so the destination is
    /// replaced only after the temp file has been flushed and renamed.
    pub fn write_to(&self, path: &Path) -> Result<(), ProgramError> {
        crate::scenario::publication::write_program_bytes(path, &self.program_bytes)
            .map_err(|error| ProgramError::Other(format!("write {}: {error}", path.display())))
    }

    /// Decode a program from its canonical on-disk bytes and re-run
    /// the same semantic invariants `normalize` enforces: exact
    /// nanosecond-aligned duration, stable boundary ordering,
    /// unique action IDs, and bounded payloads. Hashing alone does
    /// not validate the encoded program's meaning.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProgramError> {
        let envelope: WireProgram = serde_json::from_slice(bytes)
            .map_err(|error| ProgramError::Other(format!("decode envelope: {error}")))?;
        if envelope.schema_version != PROGRAM_SCHEMA_VERSION {
            return Err(ProgramError::Other(format!(
                "unsupported program schema version `{}`",
                envelope.schema_version
            )));
        }
        let quantum_micros = envelope.quantum_micros;
        let quantum = Quantum::from_micros(quantum_micros)
            .ok_or_else(|| ProgramError::Other("decoded quantum is zero".to_owned()))?;
        let duration = Duration::from_micros(envelope.duration_micros);
        let schedule = envelope
            .steps
            .into_iter()
            .map(|step| {
                Ok(ScheduleEntry {
                    boundary: step.boundary,
                    action: decode_action(step.action)?,
                })
            })
            .collect::<Result<Vec<_>, ProgramError>>()?;
        let captures = envelope
            .captures
            .into_iter()
            .map(decode_capture)
            .collect::<Result<Vec<_>, ProgramError>>()?;
        // Re-run the one validation entry point. This is the same
        // check that authors see when they build a program; the
        // decoder cannot skip it.
        Self::normalize(
            envelope.scenario_name,
            quantum,
            duration,
            schedule,
            captures,
        )
    }
}

fn decode_action(action: WireAction) -> Result<Action, ProgramError> {
    Ok(match action {
        WireAction::Setpoint {
            target_instance,
            consumer_name,
            consumer_service,
            consumer_method,
            consumer_kind,
            consumer_request,
            consumer_response,
            encoded_payload_b64,
            validity,
        } => {
            let validity = match validity.as_str() {
                "permanent" => Validity::Permanent,
                other => {
                    return Err(ProgramError::Other(format!(
                        "decoded setpoint declares unknown validity `{other}`"
                    )));
                }
            };
            Action::Setpoint {
                target_instance,
                consumer_signature: port_signature(
                    &consumer_name,
                    &consumer_service,
                    &consumer_method,
                    &consumer_kind,
                    &consumer_request,
                    &consumer_response,
                )?,
                encoded_payload: BASE64_ENGINE
                    .decode(&encoded_payload_b64)
                    .map_err(|error| ProgramError::Other(format!("setpoint payload: {error}")))?,
                validity,
            }
        }
        WireAction::Withdraw {
            target_instance,
            producer_name,
            producer_service,
            producer_method,
            producer_kind,
            producer_request,
            producer_response,
        } => Action::Withdraw {
            target_instance,
            producer_signature: port_signature(
                &producer_name,
                &producer_service,
                &producer_method,
                &producer_kind,
                &producer_request,
                &producer_response,
            )?,
        },
        WireAction::Command {
            target_instance,
            service_name,
            service_service,
            service_method,
            service_kind,
            service_request,
            service_response,
            request_encoded_b64,
            label,
            simulated_deadline_micros,
            host_deadline_micros,
        } => Action::Command {
            target_instance,
            service_signature: port_signature(
                &service_name,
                &service_service,
                &service_method,
                &service_kind,
                &service_request,
                &service_response,
            )?,
            request_encoded: BASE64_ENGINE
                .decode(&request_encoded_b64)
                .map_err(|error| ProgramError::Other(format!("command request: {error}")))?,
            label,
            simulated_deadline: Duration::from_micros(simulated_deadline_micros),
            host_deadline: Duration::from_micros(host_deadline_micros),
        },
    })
}

fn decode_capture(capture: WireCapture) -> Result<crate::scenario::plan::Capture, ProgramError> {
    Ok(match capture {
        WireCapture::State {
            name,
            signature_name,
            signature_service,
            signature_method,
            signature_kind,
            signature_request,
            signature_response,
        } => crate::scenario::plan::Capture::State {
            name,
            signature: port_signature(
                &signature_name,
                &signature_service,
                &signature_method,
                &signature_kind,
                &signature_request,
                &signature_response,
            )?,
        },
        WireCapture::Sample {
            name,
            signature_name,
            signature_service,
            signature_method,
            signature_kind,
            signature_request,
            signature_response,
        } => crate::scenario::plan::Capture::Sample {
            name,
            signature: port_signature(
                &signature_name,
                &signature_service,
                &signature_method,
                &signature_kind,
                &signature_request,
                &signature_response,
            )?,
        },
        WireCapture::Event {
            name,
            signature_name,
            signature_service,
            signature_method,
            signature_kind,
            signature_request,
            signature_response,
        } => crate::scenario::plan::Capture::Event {
            name,
            signature: port_signature(
                &signature_name,
                &signature_service,
                &signature_method,
                &signature_kind,
                &signature_request,
                &signature_response,
            )?,
        },
        WireCapture::NativeBody { name, units, frame } => {
            crate::scenario::plan::Capture::native_body(name, units, frame)
                .map_err(|error| ProgramError::Other(format!("native body capture: {error}")))?
        }
    })
}

fn port_signature(
    name: &str,
    service: &str,
    method: &str,
    kind: &str,
    request: &str,
    response: &str,
) -> Result<crate::port::PortSignature, ProgramError> {
    // `PortSignature::new_owned` owns the borrowed wire metadata and
    // delegates the lifetime promotion to the port crate. The decoder
    // does not reach for `Box::leak` directly.
    Ok(crate::port::PortSignature::new_owned(
        name,
        service,
        method,
        decode_port_kind(kind)?,
        request,
        response,
    ))
}

fn decode_port_kind(kind: &str) -> Result<crate::port::PortKind, ProgramError> {
    match kind {
        "state" => Ok(crate::port::PortKind::State),
        "sample" => Ok(crate::port::PortKind::Sample),
        "event" => Ok(crate::port::PortKind::Event),
        "stream" => Ok(crate::port::PortKind::Stream),
        "setpoint" => Ok(crate::port::PortKind::Setpoint),
        "read" => Ok(crate::port::PortKind::Read),
        "commands" => Ok(crate::port::PortKind::Commands),
        other => Err(ProgramError::Other(format!(
            "decoded program declares unknown port kind `{other}`"
        ))),
    }
}

fn check_payload(step_label: &str, payload: &[u8]) -> Result<(), ProgramError> {
    if payload.is_empty() {
        return Err(ProgramError::EmptyPayload {
            step_label: step_label.to_owned(),
        });
    }
    if payload.len() > MAX_PAYLOAD {
        return Err(ProgramError::PayloadTooLarge {
            step_label: step_label.to_owned(),
            bytes: payload.len(),
        });
    }
    Ok(())
}

fn check_target_instance(step_label: &str, target_instance: &str) -> Result<(), ProgramError> {
    if target_instance.is_empty() {
        return Err(ProgramError::Other(format!(
            "step `{step_label}` carries an empty target instance"
        )));
    }
    Ok(())
}

fn port_kind_label(kind: crate::port::PortKind) -> &'static str {
    match kind {
        crate::port::PortKind::State => "state",
        crate::port::PortKind::Sample => "sample",
        crate::port::PortKind::Event => "event",
        crate::port::PortKind::Stream => "stream",
        crate::port::PortKind::Setpoint => "setpoint",
        crate::port::PortKind::Read => "read",
        crate::port::PortKind::Commands => "commands",
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct WireProgram {
    schema_version: u32,
    scenario_name: String,
    quantum_micros: u32,
    duration_micros: u64,
    steps: Vec<WireStep>,
    captures: Vec<WireCapture>,
}

/// Wire-friendly mirror of [`Step`] that does not depend on
/// `phoxal-port` types being `Serialize`. The signature fields are
/// captured as plain strings so the on-disk JSON is stable across
/// downstream refactors of the port module.
#[derive(Debug, Serialize, Deserialize)]
struct WireStep {
    label: String,
    boundary: u32,
    action: WireAction,
}

#[derive(Debug, Serialize, Deserialize)]
enum WireAction {
    Setpoint {
        target_instance: String,
        consumer_name: String,
        consumer_service: String,
        consumer_method: String,
        consumer_kind: String,
        consumer_request: String,
        consumer_response: String,
        encoded_payload_b64: String,
        validity: String,
    },
    Withdraw {
        target_instance: String,
        producer_name: String,
        producer_service: String,
        producer_method: String,
        producer_kind: String,
        producer_request: String,
        producer_response: String,
    },
    Command {
        target_instance: String,
        service_name: String,
        service_service: String,
        service_method: String,
        service_kind: String,
        service_request: String,
        service_response: String,
        request_encoded_b64: String,
        label: String,
        simulated_deadline_micros: u64,
        host_deadline_micros: u64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
enum WireCapture {
    State {
        name: String,
        signature_name: String,
        signature_service: String,
        signature_method: String,
        signature_kind: String,
        signature_request: String,
        signature_response: String,
    },
    Sample {
        name: String,
        signature_name: String,
        signature_service: String,
        signature_method: String,
        signature_kind: String,
        signature_request: String,
        signature_response: String,
    },
    Event {
        name: String,
        signature_name: String,
        signature_service: String,
        signature_method: String,
        signature_kind: String,
        signature_request: String,
        signature_response: String,
    },
    NativeBody {
        name: String,
        units: String,
        frame: String,
    },
}

fn wire_step(step: &Step) -> WireStep {
    WireStep {
        label: step.label.clone(),
        boundary: step.boundary,
        action: match &step.action {
            Action::Setpoint {
                target_instance,
                consumer_signature,
                encoded_payload,
                validity,
            } => {
                let (name, service, method, kind, request, response) =
                    sig_strings(consumer_signature);
                WireAction::Setpoint {
                    target_instance: target_instance.clone(),
                    consumer_name: name.to_owned(),
                    consumer_service: service.to_owned(),
                    consumer_method: method.to_owned(),
                    consumer_kind: kind,
                    consumer_request: request.to_owned(),
                    consumer_response: response.to_owned(),
                    encoded_payload_b64: BASE64_ENGINE.encode(encoded_payload),
                    validity: validity.wire_label().to_owned(),
                }
            }
            Action::Withdraw {
                target_instance,
                producer_signature,
            } => {
                let (name, service, method, kind, request, response) =
                    sig_strings(producer_signature);
                WireAction::Withdraw {
                    target_instance: target_instance.clone(),
                    producer_name: name.to_owned(),
                    producer_service: service.to_owned(),
                    producer_method: method.to_owned(),
                    producer_kind: kind,
                    producer_request: request.to_owned(),
                    producer_response: response.to_owned(),
                }
            }
            Action::Command {
                target_instance,
                service_signature,
                request_encoded,
                label,
                simulated_deadline,
                host_deadline,
            } => {
                let (name, service, method, kind, request, response) =
                    sig_strings(service_signature);
                WireAction::Command {
                    target_instance: target_instance.clone(),
                    service_name: name.to_owned(),
                    service_service: service.to_owned(),
                    service_method: method.to_owned(),
                    service_kind: kind,
                    service_request: request.to_owned(),
                    service_response: response.to_owned(),
                    request_encoded_b64: BASE64_ENGINE.encode(request_encoded),
                    label: label.clone(),
                    simulated_deadline_micros: u64::try_from(simulated_deadline.as_micros())
                        .unwrap_or(0),
                    host_deadline_micros: u64::try_from(host_deadline.as_micros()).unwrap_or(0),
                }
            }
        },
    }
}

fn wire_capture(capture: &Capture) -> WireCapture {
    match capture {
        Capture::State { name, signature } => {
            let (sname, ssvc, smethod, skind, sreq, sresp) = sig_strings(signature);
            WireCapture::State {
                name: name.clone(),
                signature_name: sname.to_owned(),
                signature_service: ssvc.to_owned(),
                signature_method: smethod.to_owned(),
                signature_kind: skind,
                signature_request: sreq.to_owned(),
                signature_response: sresp.to_owned(),
            }
        }
        Capture::Sample { name, signature } => {
            let (sname, ssvc, smethod, skind, sreq, sresp) = sig_strings(signature);
            WireCapture::Sample {
                name: name.clone(),
                signature_name: sname.to_owned(),
                signature_service: ssvc.to_owned(),
                signature_method: smethod.to_owned(),
                signature_kind: skind,
                signature_request: sreq.to_owned(),
                signature_response: sresp.to_owned(),
            }
        }
        Capture::Event { name, signature } => {
            let (sname, ssvc, smethod, skind, sreq, sresp) = sig_strings(signature);
            WireCapture::Event {
                name: name.clone(),
                signature_name: sname.to_owned(),
                signature_service: ssvc.to_owned(),
                signature_method: smethod.to_owned(),
                signature_kind: skind,
                signature_request: sreq.to_owned(),
                signature_response: sresp.to_owned(),
            }
        }
        Capture::NativeBody { name, units, frame } => WireCapture::NativeBody {
            name: name.clone(),
            units: units.clone(),
            frame: frame.clone(),
        },
    }
}

fn sig_strings(
    sig: &crate::port::PortSignature,
) -> (
    &'static str,
    &'static str,
    &'static str,
    String,
    &'static str,
    &'static str,
) {
    (
        sig.name,
        sig.service,
        sig.method,
        port_kind_label(sig.kind).to_owned(),
        sig.request,
        sig.response,
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn setpoint_sig() -> crate::port::PortSignature {
        crate::port::PortSignature::new(
            "motion/cmd",
            "phoxal.motion",
            "Set",
            crate::port::PortKind::Setpoint,
            "SetpointRequest",
            "SetpointReply",
        )
    }

    fn make_setpoint(byte: u8) -> Action {
        Action::Setpoint {
            target_instance: "motion_target".to_owned(),
            consumer_signature: setpoint_sig(),
            encoded_payload: vec![byte],
            validity: crate::scenario::plan::Validity::Permanent,
        }
    }

    #[test]
    fn quantum_rejects_zero() {
        assert!(Quantum::from_micros(0).is_none());
        assert!(Quantum::from_micros(2_000).is_some());
    }

    #[test]
    fn transition_count_derives_from_duration() {
        let quantum = Quantum::from_micros(2_000).expect("quantum");
        // Six seconds at two millisecond quantum is three thousand
        // transitions, regardless of how many actions the author wrote.
        assert_eq!(
            quantum.transition_count(Duration::from_secs(6)),
            Some(3_000)
        );
        // One second at two ms = 500 transitions.
        assert_eq!(
            quantum.transition_count(Duration::from_millis(1_000)),
            Some(500)
        );
        // Unaligned durations are rejected.
        assert_eq!(quantum.transition_count(Duration::from_micros(2_001)), None);
        assert_eq!(quantum.transition_count(Duration::ZERO), None);
        // 2 ms plus 1 ns is NOT aligned at any sub-microsecond
        // resolution. The previous `as_micros()` truncation made
        // this accepted as one 2 ms transition; nanosecond
        // arithmetic must reject it.
        let unaligned = Duration::from_millis(2) + Duration::from_nanos(1);
        assert_eq!(quantum.transition_count(unaligned), None);
    }

    #[test]
    fn program_records_identity_against_stored_bytes() {
        let quantum = Quantum::from_micros(2_000).expect("quantum");
        let action = Action::Setpoint {
            target_instance: "motion_target".to_owned(),
            consumer_signature: setpoint_sig(),
            encoded_payload: vec![0xAB, 0xCD],
            validity: crate::scenario::plan::Validity::Permanent,
        };
        let program = Program::normalize(
            "scenarios/Demo",
            quantum,
            Duration::from_secs(6),
            vec![ScheduleEntry::at(0, action.clone())],
            vec![],
        )
        .expect("normalize");
        // 6 s / 2 ms = 3,000 transitions, independent of action count.
        assert_eq!(program.transition_count(), 3_000);
        program.verify_identity().expect("identity");
        let stored_bytes = program.program_bytes().to_vec();
        let stored_digest = program.program_digest().to_owned();
        let recomputed = sha256_hex(&stored_bytes);
        assert_eq!(recomputed, stored_digest);
    }

    #[test]
    fn normalize_stable_sorts_authored_boundaries() {
        // Regression: the old normalize iterated schedule in authored
        // order, so [500, 1] remained [500, 1] rather than [1, 500].
        // The new path stable-sorts by boundary while preserving the
        // authored order at ties.
        let quantum = Quantum::from_micros(2_000).expect("quantum");
        let make = make_setpoint;
        let program = Program::normalize(
            "scenarios/Sort",
            quantum,
            Duration::from_secs(6),
            vec![
                ScheduleEntry::at(500, make(1)),
                ScheduleEntry::at(1, make(2)),
                ScheduleEntry::at(500, make(3)),
            ],
            vec![],
        )
        .expect("normalize");
        let payload_bytes: Vec<Vec<u8>> = program
            .steps()
            .iter()
            .filter_map(|step| match &step.action {
                Action::Setpoint {
                    encoded_payload, ..
                } => Some(encoded_payload.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(payload_bytes, vec![vec![2], vec![1], vec![3]]);
        // Each step must carry a distinct label even when two share
        // the same boundary.
        let labels: Vec<&str> = program
            .steps()
            .iter()
            .map(|step| step.label.as_str())
            .collect();
        assert!(labels[0] != labels[1]);
        assert!(labels[1] != labels[2]);
    }

    #[test]
    fn equal_boundaries_retain_authored_order() {
        let quantum = Quantum::from_micros(2_000).expect("quantum");
        let make = make_setpoint;
        let program = Program::normalize(
            "scenarios/Order",
            quantum,
            Duration::from_secs(6),
            vec![
                ScheduleEntry::at(100, make(1)),
                ScheduleEntry::at(100, make(2)),
                ScheduleEntry::at(200, make(3)),
            ],
            vec![],
        )
        .expect("normalize");
        let payload_bytes: Vec<Vec<u8>> = program
            .steps()
            .iter()
            .filter_map(|step| match &step.action {
                Action::Setpoint {
                    encoded_payload, ..
                } => Some(encoded_payload.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(payload_bytes, vec![vec![1], vec![2], vec![3]]);
    }

    #[test]
    fn decode_round_trip_preserves_typed_fields() {
        // Build a six-second sparse schedule with a single action
        // at boundary 500 — the canonical item 4 acceptance probe.
        // The wire form must preserve target instance, validity,
        // and the typed payload so the decoded program is identical
        // to what the author wrote.
        let quantum = Quantum::from_micros(2_000).expect("quantum");
        let action = Action::Setpoint {
            target_instance: "motion_target".to_owned(),
            consumer_signature: setpoint_sig(),
            encoded_payload: vec![0xAB, 0xCD],
            validity: crate::scenario::plan::Validity::Permanent,
        };
        let program = Program::normalize(
            "scenarios/Sparse",
            quantum,
            Duration::from_secs(6),
            vec![ScheduleEntry::at(500, action.clone())],
            vec![],
        )
        .expect("normalize");
        let decoded = Program::decode(program.program_bytes()).expect("decode");
        decoded.verify_identity().expect("identity");
        assert_eq!(decoded.transition_count(), 3_000);
        assert_eq!(decoded.scenario_name(), "scenarios/Sparse");
        let step = &decoded.steps()[0];
        match &step.action {
            Action::Setpoint {
                target_instance,
                encoded_payload,
                validity,
                ..
            } => {
                assert_eq!(target_instance, "motion_target");
                assert_eq!(encoded_payload, &vec![0xAB, 0xCD]);
                assert_eq!(*validity, crate::scenario::plan::Validity::Permanent);
            }
            other => panic!("expected setpoint, got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_unknown_validity() {
        // Tampered wire form must fail at decode, not at run time.
        let quantum = Quantum::from_micros(2_000).expect("quantum");
        let action = Action::Setpoint {
            target_instance: "motion_target".to_owned(),
            consumer_signature: setpoint_sig(),
            encoded_payload: vec![0xAB, 0xCD],
            validity: crate::scenario::plan::Validity::Permanent,
        };
        let program = Program::normalize(
            "scenarios/TamperedValidity",
            quantum,
            Duration::from_secs(2),
            vec![ScheduleEntry::at(0, action)],
            vec![],
        )
        .expect("normalize");
        let mut bytes = program.program_bytes().to_vec();
        let text = std::str::from_utf8(&bytes)
            .expect("utf-8 envelope")
            .to_owned();
        let tampered = text.replace("\"permanent\"", "\"forever\"");
        bytes = tampered.into_bytes();
        assert!(Program::decode(&bytes).is_err());
    }

    #[test]
    fn decode_rejects_unknown_port_kind() {
        let quantum = Quantum::from_micros(2_000).expect("quantum");
        let action = Action::Setpoint {
            target_instance: "motion_target".to_owned(),
            consumer_signature: setpoint_sig(),
            encoded_payload: vec![0xAB],
            validity: crate::scenario::plan::Validity::Permanent,
        };
        let program = Program::normalize(
            "scenarios/TamperedKind",
            quantum,
            Duration::from_secs(2),
            vec![ScheduleEntry::at(0, action)],
            vec![],
        )
        .expect("normalize");
        let mut bytes = program.program_bytes().to_vec();
        let text = std::str::from_utf8(&bytes)
            .expect("utf-8 envelope")
            .to_owned();
        let tampered = text.replace("\"setpoint\"", "\"nonsense\"");
        bytes = tampered.into_bytes();
        assert!(Program::decode(&bytes).is_err());
    }
}
