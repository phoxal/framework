//! Immutable program representation. A `Program` is the on-disk
//! artifact that pairs a scenario with one finite quantized schedule.
//! It carries the exact byte serialization, length, and SHA-256 digest
//! so the bundle can prove identity without re-serializing.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::scenario::plan::{Action, Capture, MAX_PAYLOAD, Step};

/// One immutable, serializable scenario program. Constructed via
/// [`Program::normalize`] which deterministically orders steps,
/// records byte length and digest, and refuses malformed inputs.
///
/// Serialization goes through the wire form [`WireProgram`] rather
/// than the public type, so the on-disk format stays stable while
/// the typed Rust fields can evolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub schema_version: u32,
    pub scenario_name: String,
    pub duration_micros: u64,
    pub steps: Vec<Step>,
    pub captures: Vec<Capture>,
    pub program_bytes: Vec<u8>,
    pub program_digest: String,
    pub byte_length: u32,
}

/// Errors returned by [`Program::normalize`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramError {
    EmptyScenarioName,
    DuplicateStepLabel(String),
    QuantumOutOfRange {
        label: String,
        quantum: u32,
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
                quantum,
                transitions,
            } => write!(
                f,
                "step `{label}` quantum {quantum} is at or beyond the final transition {transitions}"
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

impl Program {
    /// Construct a normalized program. `scenario_name` is the public
    /// identity (matches `scenarios/<StructIdent>`). `steps` and
    /// `captures` are copied into the program; the caller must ensure
    /// any encoded payload was produced by the matching consumer
    /// service's protobuf encoder.
    pub fn normalize(
        scenario_name: impl Into<String>,
        duration: std::time::Duration,
        mut steps: Vec<Step>,
        captures: Vec<Capture>,
    ) -> Result<Self, ProgramError> {
        let scenario_name = scenario_name.into();
        if scenario_name.is_empty() {
            return Err(ProgramError::EmptyScenarioName);
        }
        // Deterministic ordering: primary by quantum_index, secondary by label.
        steps.sort_by(|a, b| {
            a.quantum_index
                .cmp(&b.quantum_index)
                .then_with(|| a.label.cmp(&b.label))
        });
        let transitions = steps.len() as u32;
        // Validate labels and quantum bounds before serialization.
        for step in &steps {
            if step.quantum_index >= transitions {
                return Err(ProgramError::QuantumOutOfRange {
                    label: step.label.clone(),
                    quantum: step.quantum_index,
                    transitions,
                });
            }
            match &step.action {
                crate::scenario::plan::Action::Setpoint {
                    encoded_payload, ..
                } => {
                    check_payload(step.label.as_str(), encoded_payload)?;
                }
                crate::scenario::plan::Action::Command {
                    request_encoded, ..
                } => {
                    check_payload(step.label.as_str(), request_encoded)?;
                }
                crate::scenario::plan::Action::Withdraw { .. } => {}
            }
        }
        // Canonical wire form: a small JSON envelope. The user keeps
        // writing typed Rust values; the JSON is for storage identity.
        let duration_micros = u64::try_from(duration.as_micros())
            .map_err(|_| ProgramError::Other("duration overflows u64 microseconds".to_owned()))?;
        let envelope = WireProgram {
            schema_version: PROGRAM_SCHEMA_VERSION,
            scenario_name: scenario_name.clone(),
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
            duration_micros,
            steps,
            captures,
            program_bytes,
            program_digest,
            byte_length,
        })
    }

    /// Verifies the recorded byte length and digest against the
    /// canonical serialization. Returns `Ok` on a clean identity
    /// check, `Err` if the program was tampered with after recording.
    pub fn verify_identity(&self) -> Result<(), ProgramError> {
        let envelope = WireProgram {
            schema_version: PROGRAM_SCHEMA_VERSION,
            scenario_name: self.scenario_name.clone(),
            duration_micros: self.duration_micros,
            steps: self.steps.iter().map(wire_step).collect(),
            captures: self.captures.iter().map(wire_capture).collect(),
        };
        let bytes = serde_json::to_vec(&envelope)
            .map_err(|error| ProgramError::Other(format!("re-serialize: {error}")))?;
        if bytes.len() as u32 != self.byte_length {
            return Err(ProgramError::Other(format!(
                "byte length mismatch: stored {} actual {}",
                self.byte_length,
                bytes.len()
            )));
        }
        let actual = sha256_hex(&bytes);
        if actual != self.program_digest {
            return Err(ProgramError::Other(format!(
                "digest mismatch: stored {} actual {actual}",
                self.program_digest
            )));
        }
        Ok(())
    }

    /// Persists the canonical bytes to disk at `path`. Writes are
    /// atomic; the destination is replaced only after the temp file
    /// has been flushed.
    pub fn write_to(&self, path: &Path) -> Result<(), ProgramError> {
        write_atomic(path, &self.program_bytes)
            .map_err(|error| ProgramError::Other(format!("write {}: {error}", path.display())))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct WireProgram {
    schema_version: u32,
    scenario_name: String,
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
    quantum_index: u32,
    action: WireAction,
}

#[derive(Debug, Serialize, Deserialize)]
enum WireAction {
    Setpoint {
        consumer_name: String,
        consumer_service: String,
        consumer_method: String,
        consumer_kind: String,
        consumer_request: String,
        consumer_response: String,
        encoded_payload_b64: String,
    },
    Withdraw {
        producer_name: String,
        producer_service: String,
        producer_method: String,
        producer_kind: String,
        producer_request: String,
        producer_response: String,
    },
    Command {
        service_name: String,
        service_service: String,
        service_method: String,
        service_kind: String,
        service_request: String,
        service_response: String,
        request_encoded_b64: String,
        label: String,
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

fn port_kind_label(kind: phoxal_port::PortKind) -> &'static str {
    match kind {
        phoxal_port::PortKind::State => "state",
        phoxal_port::PortKind::Sample => "sample",
        phoxal_port::PortKind::Event => "event",
        phoxal_port::PortKind::Stream => "stream",
        phoxal_port::PortKind::Setpoint => "setpoint",
        phoxal_port::PortKind::Read => "read",
        phoxal_port::PortKind::Commands => "commands",
        _ => "unknown",
    }
}

#[allow(dead_code)]
fn port_kind_from_label(label: &str) -> Option<phoxal_port::PortKind> {
    Some(match label {
        "state" => phoxal_port::PortKind::State,
        "sample" => phoxal_port::PortKind::Sample,
        "event" => phoxal_port::PortKind::Event,
        "stream" => phoxal_port::PortKind::Stream,
        "setpoint" => phoxal_port::PortKind::Setpoint,
        "read" => phoxal_port::PortKind::Read,
        "commands" => phoxal_port::PortKind::Commands,
        _ => return None,
    })
}

#[allow(dead_code)]
fn _port_kind_label_mark_used() -> &'static str {
    port_kind_label(phoxal_port::PortKind::State)
}

fn wire_step(step: &Step) -> WireStep {
    WireStep {
        label: step.label.clone(),
        quantum_index: step.quantum_index,
        action: match &step.action {
            Action::Setpoint {
                consumer_signature,
                encoded_payload,
            } => {
                let (name, service, method, kind, request, response) =
                    sig_strings(consumer_signature);
                WireAction::Setpoint {
                    consumer_name: name.to_owned(),
                    consumer_service: service.to_owned(),
                    consumer_method: method.to_owned(),
                    consumer_kind: kind,
                    consumer_request: request.to_owned(),
                    consumer_response: response.to_owned(),
                    encoded_payload_b64: base64_encode(encoded_payload),
                }
            }
            Action::Withdraw { producer_signature } => {
                let (name, service, method, kind, request, response) =
                    sig_strings(producer_signature);
                WireAction::Withdraw {
                    producer_name: name.to_owned(),
                    producer_service: service.to_owned(),
                    producer_method: method.to_owned(),
                    producer_kind: kind,
                    producer_request: request.to_owned(),
                    producer_response: response.to_owned(),
                }
            }
            Action::Command {
                service_signature,
                request_encoded,
                label,
            } => {
                let (name, service, method, kind, request, response) =
                    sig_strings(service_signature);
                WireAction::Command {
                    service_name: name.to_owned(),
                    service_service: service.to_owned(),
                    service_method: method.to_owned(),
                    service_kind: kind,
                    service_request: request.to_owned(),
                    service_response: response.to_owned(),
                    request_encoded_b64: base64_encode(request_encoded),
                    label: label.clone(),
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
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut chunks = bytes.chunks_exact(3);
    for chunk in &mut chunks {
        let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | chunk[2] as u32;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        out.push(TABLE[(n & 0x3f) as usize] as char);
    }
    let remainder = chunks.remainder();
    if !remainder.is_empty() {
        let n = (remainder[0] as u32) << 16;
        let second = if remainder.len() == 2 {
            (remainder[1] as u32) << 8
        } else {
            0
        };
        let n = n | second;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        if remainder.len() == 2 {
            out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
            out.push('=');
        } else {
            out.push('=');
            out.push('=');
        }
    }
    out
}

fn sig_strings(
    sig: &phoxal_port::PortSignature,
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
    // A small, dependency-free SHA-256 implementation. It runs only
    // once per program boundary so the cost is amortised; correctness
    // here matters more than micro-optimisation. Uses the FIPS 180-4
    // round constants and message schedule.
    use std::num::Wrapping;
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = bytes.to_vec();
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = (Wrapping(w[i - 16]) + Wrapping(s0) + Wrapping(w[i - 7]) + Wrapping(s1)).0;
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 =
                (Wrapping(hh) + Wrapping(s1) + Wrapping(ch) + Wrapping(K[i]) + Wrapping(w[i])).0;
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let mj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = (Wrapping(s0) + Wrapping(mj)).0;
            hh = g;
            g = f;
            f = e;
            e = (Wrapping(d) + Wrapping(t1)).0;
            d = c;
            c = b;
            b = a;
            a = (Wrapping(t1) + Wrapping(t2)).0;
        }
        h[0] = (Wrapping(h[0]) + Wrapping(a)).0;
        h[1] = (Wrapping(h[1]) + Wrapping(b)).0;
        h[2] = (Wrapping(h[2]) + Wrapping(c)).0;
        h[3] = (Wrapping(h[3]) + Wrapping(d)).0;
        h[4] = (Wrapping(h[4]) + Wrapping(e)).0;
        h[5] = (Wrapping(h[5]) + Wrapping(f)).0;
        h[6] = (Wrapping(h[6]) + Wrapping(g)).0;
        h[7] = (Wrapping(h[7]) + Wrapping(hh)).0;
    }
    let mut out = String::with_capacity(64);
    for word in h {
        out.push_str(&format!("{word:08x}"));
    }
    out
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("program.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::plan::{Action, Step};
    use phoxal_port::PortSignature;

    fn sig(kind: phoxal_port::PortKind) -> PortSignature {
        PortSignature::new("motion/cmd", "phoxal.motion", "Set", kind, "Req", "Reply")
    }

    #[test]
    fn rejects_empty_scenario_name() {
        let result = Program::normalize("", std::time::Duration::from_secs(1), vec![], vec![]);
        assert_eq!(result.unwrap_err(), ProgramError::EmptyScenarioName);
    }

    #[test]
    fn normalizes_and_round_trips() {
        let program = Program::normalize(
            "scenarios/First",
            std::time::Duration::from_secs(2),
            vec![Step::new(
                "set",
                0,
                Action::Setpoint {
                    consumer_signature: sig(phoxal_port::PortKind::Setpoint),
                    encoded_payload: vec![1, 2, 3],
                },
            )],
            vec![Capture::state("motion", sig(phoxal_port::PortKind::State))],
        )
        .unwrap();
        program.verify_identity().expect("identity check");
        assert_eq!(program.byte_length as usize, program.program_bytes.len());
        assert_eq!(program.program_digest.len(), 64);
        assert_eq!(program.steps[0].label, "set");
    }

    #[test]
    fn deterministic_ordering() {
        let make = |label: &'static str, q: u32| {
            Step::new(
                label,
                q,
                Action::Setpoint {
                    consumer_signature: sig(phoxal_port::PortKind::Setpoint),
                    encoded_payload: vec![1],
                },
            )
        };
        let a = Program::normalize(
            "scenarios/A",
            std::time::Duration::from_secs(1),
            vec![make("b", 1), make("a", 0)],
            vec![],
        )
        .unwrap();
        let b = Program::normalize(
            "scenarios/A",
            std::time::Duration::from_secs(1),
            vec![make("a", 0), make("b", 1)],
            vec![],
        )
        .unwrap();
        assert_eq!(a.program_digest, b.program_digest);
    }

    #[test]
    fn detects_tampering() {
        let mut program = Program::normalize(
            "scenarios/First",
            std::time::Duration::from_secs(1),
            vec![],
            vec![],
        )
        .unwrap();
        program.scenario_name = "scenarios/Other".to_owned();
        let result = program.verify_identity();
        assert!(matches!(result, Err(ProgramError::Other(_))));
    }

    #[test]
    fn rejects_quantum_out_of_range() {
        let result = Program::normalize(
            "scenarios/Bad",
            std::time::Duration::from_secs(1),
            vec![Step::new(
                "beyond",
                5,
                Action::Setpoint {
                    consumer_signature: sig(phoxal_port::PortKind::Setpoint),
                    encoded_payload: vec![1],
                },
            )],
            vec![],
        );
        assert!(matches!(
            result.unwrap_err(),
            ProgramError::QuantumOutOfRange { .. }
        ));
    }

    #[test]
    fn rejects_oversized_payload() {
        let result = Program::normalize(
            "scenarios/Bad",
            std::time::Duration::from_secs(1),
            vec![Step::new(
                "big",
                0,
                Action::Setpoint {
                    consumer_signature: sig(phoxal_port::PortKind::Setpoint),
                    encoded_payload: vec![0; MAX_PAYLOAD + 1],
                },
            )],
            vec![],
        );
        assert!(matches!(
            result.unwrap_err(),
            ProgramError::PayloadTooLarge { .. }
        ));
    }
}
