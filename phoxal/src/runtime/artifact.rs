//! Bounded runtime contract records retained in native artifacts.
//!
//! The record is deliberately a small JSON document assembled at compile
//! time. It contains only source-authored timing, configuration, binding, and
//! capacity facts. It is not a schema AST and it never executes service code.

use super::RuntimeSpec;
use super::input::{InputField, InputKind};
use super::outputs::{OutputField, OutputKind};

/// Eight-byte prefix used to locate a record inside a native section.
pub const ARTIFACT_MAGIC: [u8; 8] = *b"PHXART0\n";

/// Maximum encoded record size retained by one runtime binary.
pub const ARTIFACT_RECORD_CAPACITY: usize = 65_536;

/// Schema identifier for the native runtime contract record.
///
/// The `const fn` `runtime_record` writes this string verbatim into the
/// artifact bytes because it is invoked from a `static` initializer in
/// `phoxal-macros` and cannot call into serde. The same string lives on the
/// [`crate::artifact::RuntimeRecord`] enum's `V0` variant as the
/// `#[serde(rename = ...)]` discriminator. The duplication is forced by the
/// `const fn` contract; serde's `rename` attribute only accepts string
/// literals and `runtime_record` cannot borrow the enum's discriminator.
const ARTIFACT_SCHEMA: &str = "phoxal/artifact/v0";

/// A fixed-capacity, length-delimited native artifact record.
///
/// The fixed backing array keeps the record available to a link section
/// without a heap allocation or a build-time schema encoder.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ArtifactRecord {
    /// Number of meaningful bytes in the bytes field.
    pub len: u32,
    /// Magic prefix, length, and UTF-8 JSON payload.
    pub bytes: [u8; ARTIFACT_RECORD_CAPACITY],
}

impl ArtifactRecord {
    /// Returns the meaningful record bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

/// Builds the one runtime record retained by a registered service.
pub const fn runtime_record(
    spec: RuntimeSpec,
    config_schema: &str,
    inputs: &[InputField],
    transient_outputs: &[OutputField],
    service_outputs: &[OutputField],
) -> ArtifactRecord {
    let mut record = RecordBuilder::new();
    record.push_bytes(&ARTIFACT_MAGIC);
    let length_start = record.position();
    record.push_bytes(&[0, 0, 0, 0]);
    record.push_str("{\"schema\":");
    record.push_quoted(ARTIFACT_SCHEMA);
    record.push_str(",\"record\":\"runtime\",\"period_ms\":");
    record.push_u64(spec.period.as_millis());
    record.push_str(",\"timeout_ms\":");
    record.push_u64(spec.timeout.as_millis());
    record.push_str(",\"init_timeout_ms\":");
    record.push_u64(spec.init_timeout.as_millis());
    record.push_str(",\"config_schema\":");
    record.push_raw_json(config_schema);
    record.push_str(",\"inputs\":[");
    record.push_inputs(inputs);
    record.push_str("],\"transient_outputs\":[");
    record.push_outputs(transient_outputs);
    record.push_str("],\"service_outputs\":[");
    record.push_outputs(service_outputs);
    record.push_str("]}");
    record.push_byte(b'\n');
    record.write_length(length_start);
    record.into_record()
}

struct RecordBuilder {
    bytes: [u8; ARTIFACT_RECORD_CAPACITY],
    position: usize,
}

impl RecordBuilder {
    const fn new() -> Self {
        Self {
            bytes: [0; ARTIFACT_RECORD_CAPACITY],
            position: 0,
        }
    }

    const fn push_byte(&mut self, byte: u8) {
        assert!(
            self.position < ARTIFACT_RECORD_CAPACITY,
            "phoxal runtime artifact record exceeds 64 KiB"
        );
        self.bytes[self.position] = byte;
        self.position += 1;
    }

    const fn push_bytes(&mut self, bytes: &[u8]) {
        let mut index = 0;
        while index < bytes.len() {
            self.push_byte(bytes[index]);
            index += 1;
        }
    }

    const fn push_str(&mut self, value: &str) {
        self.push_bytes(value.as_bytes());
    }

    const fn push_quoted(&mut self, value: &str) {
        self.push_byte(b'"');
        self.push_escaped(value);
        self.push_byte(b'"');
    }

    const fn push_escaped(&mut self, value: &str) {
        let bytes = value.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'"' => self.push_str("\\\""),
                b'\\' => self.push_str("\\\\"),
                b'\n' => self.push_str("\\n"),
                b'\r' => self.push_str("\\r"),
                b'\t' => self.push_str("\\t"),
                byte if byte < 0x20 => {
                    self.push_str("\\u00");
                    self.push_hex(byte >> 4);
                    self.push_hex(byte & 0x0f);
                }
                byte => self.push_byte(byte),
            }
            index += 1;
        }
    }

    const fn push_message_type(&mut self, value: Option<super::input::MessageType>) {
        match value {
            None => self.push_str("null"),
            Some(value) => {
                self.push_byte(b'"');
                if !value.package.is_empty() {
                    self.push_escaped(value.package);
                    self.push_byte(b'.');
                }
                self.push_escaped(value.name);
                self.push_byte(b'"');
            }
        }
    }

    const fn push_hex(&mut self, value: u8) {
        self.push_byte(match value {
            0..=9 => b'0' + value,
            10..=15 => b'a' + value - 10,
            _ => unreachable!(),
        });
    }

    const fn push_raw_json(&mut self, value: &str) {
        self.push_str(value);
    }

    const fn push_u64(&mut self, value: u64) {
        if value == 0 {
            self.push_byte(b'0');
            return;
        }
        let mut digits = [0_u8; 20];
        let mut length = 0;
        let mut remaining = value;
        while remaining != 0 {
            digits[length] = b'0' + (remaining % 10) as u8;
            length += 1;
            remaining /= 10;
        }
        while length != 0 {
            length -= 1;
            self.push_byte(digits[length]);
        }
    }

    const fn push_bool(&mut self, value: bool) {
        if value {
            self.push_str("true");
        } else {
            self.push_str("false");
        }
    }

    const fn push_optional_u64(&mut self, value: Option<u64>) {
        match value {
            Some(value) => self.push_u64(value),
            None => self.push_str("null"),
        }
    }

    const fn push_input_role(&mut self, kind: InputKind) {
        self.push_quoted(match kind {
            InputKind::Latest => "observation_latest",
            InputKind::Samples | InputKind::Events | InputKind::Stream => "observation_history",
            InputKind::Setpoint => "leased_value",
            InputKind::Commands => "call_ingress",
            InputKind::Read => "call_result",
            InputKind::Request => "call_target",
            InputKind::Operation => "operation_result",
            InputKind::Completions => "call_completions",
        });
    }

    const fn push_output_role(&mut self, kind: OutputKind) {
        self.push_quoted(match kind {
            OutputKind::State
            | OutputKind::Sample
            | OutputKind::Event
            | OutputKind::Stream
            | OutputKind::Setpoint
            | OutputKind::Read => "method",
            OutputKind::Reply => "reply",
            OutputKind::Activate => "activation",
            OutputKind::Operation => "operation",
        });
    }

    const fn push_signature(&mut self, signature: Option<crate::port::PortSignature>) {
        let Some(signature) = signature else {
            self.push_str("null");
            return;
        };
        self.push_str("{\"endpoint\":");
        self.push_quoted(signature.name);
        self.push_str(",\"service\":");
        self.push_quoted(signature.service);
        self.push_str(",\"method\":");
        self.push_quoted(signature.method);
        self.push_str(",\"shape\":");
        self.push_quoted(match signature.shape {
            crate::contract::MethodShape::Call => "call",
            crate::contract::MethodShape::Observation => "observation",
        });
        self.push_str(",\"request\":");
        self.push_quoted(signature.request);
        self.push_str(",\"response\":");
        self.push_quoted(signature.response);
        self.push_str(",\"retained_latest\":");
        self.push_bool(signature.retained_latest);
        self.push_str(",\"lease_valid_for_ms\":");
        self.push_optional_u64(signature.lease_valid_for_ms);
        self.push_byte(b'}');
    }

    const fn push_inputs(&mut self, fields: &[InputField]) {
        let mut index = 0;
        while index < fields.len() {
            if index != 0 {
                self.push_byte(b',');
            }
            let field = fields[index];
            self.push_str("{\"name\":");
            self.push_quoted(field.name);
            self.push_str(",\"role\":");
            self.push_input_role(field.kind);
            self.push_str(",\"max_age_ms\":");
            self.push_optional_u64(field.max_age_ms);
            self.push_str(",\"max_items\":");
            self.push_optional_u64(field.max_items);
            self.push_str(",\"max_bytes\":");
            self.push_optional_u64(field.max_bytes);
            self.push_str(",\"port\":");
            match field.port {
                Some(port) => self.push_quoted(port),
                None => self.push_str("null"),
            }
            self.push_str(",\"signature\":");
            self.push_signature(field.port_signature);
            self.push_str(",\"request_fqn\":");
            self.push_message_type(field.request_type);
            self.push_str(",\"response_fqn\":");
            self.push_message_type(field.response_type);
            self.push_byte(b'}');
            index += 1;
        }
    }

    const fn push_outputs(&mut self, fields: &[OutputField]) {
        let mut index = 0;
        while index < fields.len() {
            if index != 0 {
                self.push_byte(b',');
            }
            let field = fields[index];
            self.push_str("{\"name\":");
            self.push_quoted(field.name);
            self.push_str(",\"role\":");
            self.push_output_role(field.kind);
            self.push_str(",\"port\":");
            match field.port {
                Some(port) => self.push_quoted(port),
                None => self.push_str("null"),
            }
            self.push_str(",\"signature\":");
            self.push_signature(field.port_signature);
            self.push_str(",\"input\":");
            match field.input {
                Some(input) => self.push_quoted(input),
                None => self.push_str("null"),
            }
            self.push_str(",\"project\":");
            match field.project {
                Some(project) => self.push_quoted(project),
                None => self.push_str("null"),
            }
            self.push_str(",\"max_items\":");
            self.push_optional_u64(field.max_items);
            self.push_str(",\"max_bytes\":");
            self.push_optional_u64(field.max_bytes);
            self.push_str(",\"max_request_bytes\":");
            self.push_optional_u64(field.max_request_bytes);
            self.push_str(",\"every_steps\":");
            self.push_optional_u64(field.every_steps);
            self.push_str(",\"on_change\":");
            self.push_bool(field.on_change);
            self.push_str(",\"bootstrap\":");
            self.push_bool(field.bootstrap);
            self.push_str(",\"valid_for_ms\":");
            self.push_optional_u64(field.valid_for_ms);
            self.push_str(",\"timeout_ms\":");
            self.push_optional_u64(field.timeout_ms);
            self.push_str(",\"cancel_grace_ms\":");
            self.push_optional_u64(field.cancel_grace_ms);
            self.push_byte(b'}');
            index += 1;
        }
    }

    const fn position(&self) -> usize {
        self.position
    }

    const fn write_length(&mut self, length_start: usize) {
        let payload_length = self.position - length_start - 4;
        assert!(
            payload_length <= u32::MAX as usize,
            "phoxal runtime artifact record length exceeds u32"
        );
        let bytes = (payload_length as u32).to_le_bytes();
        let mut index = 0;
        while index < bytes.len() {
            self.bytes[length_start + index] = bytes[index];
            index += 1;
        }
    }

    const fn into_record(self) -> ArtifactRecord {
        ArtifactRecord {
            len: self.position as u32,
            bytes: self.bytes,
        }
    }
}
