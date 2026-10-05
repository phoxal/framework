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
    conversions: &str,
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
    record.push_str(",\"record\":\"runtime\"");
    record.push_str(",\"period_ms\":");
    record.push_u64(spec.period.as_millis());
    record.push_str(",\"timeout_ms\":");
    record.push_u64(spec.timeout.as_millis());
    record.push_str(",\"init_timeout_ms\":");
    record.push_u64(spec.init_timeout.as_millis());
    record.push_str(",\"config_schema\":");
    record.push_raw_json(config_schema);
    record.push_str(",\"conversions\":");
    record.push_raw_json(conversions);
    record.push_str(",\"inputs\":[");
    record.push_inputs(inputs, transient_outputs, service_outputs);
    record.push_str("],\"outputs\":[");
    let count = record.push_outputs(transient_outputs, 0);
    record.push_outputs(service_outputs, count);
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

    const fn push_input_delivery(&mut self, kind: InputKind) {
        self.push_quoted(match kind {
            InputKind::Latest => "observation_latest",
            InputKind::Samples | InputKind::Events | InputKind::Stream => "observation_history",
            InputKind::Setpoint => "leased_value",
            InputKind::Commands => "call_ingress",
            InputKind::Read => "call_result",
            InputKind::Request => "call_target",
            InputKind::Operation => panic!("local operation has no public endpoint"),
            InputKind::Completions => "call_completions",
        });
    }

    const fn push_signature(&mut self, signature: Option<crate::contracts::MethodSignature>) {
        let Some(signature) = signature else {
            self.push_str("null");
            return;
        };
        self.push_str("{\"endpoint\":");
        self.push_quoted(signature.endpoint);
        self.push_str(",\"service\":");
        self.push_quoted(signature.service);
        self.push_str(",\"method\":");
        self.push_quoted(signature.method);
        self.push_str(",\"shape\":");
        self.push_quoted(match signature.shape {
            crate::contracts::MethodShape::Call => "call",
            crate::contracts::MethodShape::Observation => "observation",
        });
        self.push_str(",\"request\":");
        self.push_quoted(signature.request);
        self.push_str(",\"response\":");
        self.push_quoted(signature.response);
        self.push_str(",\"retained_latest\":");
        self.push_bool(signature.retained_latest);
        self.push_str(",\"lease_valid_for_ms\":");
        self.push_optional_u64(match signature.lease {
            Some(lease) => Some(lease.valid_for_ms()),
            None => None,
        });
        self.push_byte(b'}');
    }

    const fn push_inputs(
        &mut self,
        fields: &[InputField],
        transient: &[OutputField],
        services: &[OutputField],
    ) {
        let mut index = 0;
        let mut emitted = 0;
        while index < fields.len() {
            let field = fields[index];
            index += 1;
            if matches!(field.kind, InputKind::Operation) {
                continue;
            }
            if emitted != 0 {
                self.push_byte(b',');
            }
            emitted += 1;
            self.push_str("{\"name\":");
            // Required endpoints belong to the consumer graph slot;
            // served call/setpoint ingress belongs to its public method.
            self.push_quoted(match (field.kind, field.port) {
                (InputKind::Commands | InputKind::Setpoint, Some(port)) => port,
                _ => field.name,
            });
            self.push_str(",\"delivery\":");
            self.push_input_delivery(field.kind);
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
            let reply = reply_bounds(field.name, transient, services);
            self.push_str(",\"response_max_bytes\":");
            self.push_optional_u64(reply.0);
            self.push_str(",\"response_max_items\":");
            self.push_optional_u64(reply.1);
            self.push_byte(b'}');
        }
    }

    const fn push_outputs(&mut self, fields: &[OutputField], mut emitted: usize) -> usize {
        let mut index = 0;
        while index < fields.len() {
            let field = fields[index];
            index += 1;
            if field.port_signature.is_none() {
                continue;
            }
            if emitted != 0 {
                self.push_byte(b',');
            }
            emitted += 1;
            self.push_str("{\"name\":");
            self.push_quoted(match field.port {
                Some(port) => port,
                None => panic!("provided method has no endpoint"),
            });
            if let Some(family) = field.family {
                self.push_str(",\"family\":{\"config_pointer\":");
                self.push_quoted(family.config_pointer);
                self.push_str(",\"suffix\":");
                self.push_quoted(family.suffix);
                self.push_str(",\"max_ports\":");
                self.push_u64(family.max_ports);
                self.push_byte(b'}');
            }
            self.push_str(",\"port\":");
            match field.port {
                Some(port) => self.push_quoted(port),
                None => self.push_str("null"),
            }
            self.push_str(",\"signature\":");
            self.push_signature(field.port_signature);
            self.push_str(",\"max_items\":");
            self.push_optional_u64(field.max_items);
            self.push_str(",\"max_bytes\":");
            self.push_optional_u64(field.max_bytes);
            self.push_str(",\"max_request_bytes\":");
            self.push_optional_u64(field.max_request_bytes);
            self.push_str(",\"every_steps\":");
            self.push_optional_u64(field.every_steps);
            self.push_str(",\"bootstrap\":");
            self.push_bool(field.bootstrap);
            self.push_str(",\"timeout_ms\":");
            self.push_optional_u64(field.timeout_ms);
            self.push_byte(b'}');
        }
        emitted
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

// Private Rust reply association is resolved here and never serialized.
const fn reply_bounds(
    name: &str,
    transient: &[OutputField],
    services: &[OutputField],
) -> (Option<u64>, Option<u64>) {
    let groups = [transient, services];
    let mut group = 0;
    let mut found = false;
    let mut bounds = (None, None);
    while group < groups.len() {
        let mut index = 0;
        while index < groups[group].len() {
            let output = groups[group][index];
            if matches!(output.kind, OutputKind::Reply)
                && let Some(input) = output.input
                && same_name(name, input)
            {
                assert!(!found, "call ingress has multiple reply bounds");
                found = true;
                bounds = (output.max_bytes, output.max_items);
            }
            index += 1;
        }
        group += 1;
    }
    bounds
}
const fn same_name(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_contract_resolves_private_call_associations_and_hides_local_operations() {
        let signature = crate::contracts::MethodSignature::new(
            "fixture.Service",
            "Command",
            "command",
            crate::contracts::MethodShape::Call,
            "fixture.Request",
            "fixture.Response",
            false,
            None,
            &[],
        );
        let input = InputField {
            name: "private_command_queue",
            kind: InputKind::Commands,
            max_age_ms: None,
            max_items: Some(2),
            max_bytes: Some(128),
            port: Some("command"),
            port_signature: Some(signature),
            request_type: None,
            response_type: None,
        };
        let reply = OutputField {
            family: None,
            name: "private_reply_batch",
            kind: OutputKind::Reply,
            port: None,
            port_signature: None,
            input: Some(input.name),
            project: None,
            max_items: Some(2),
            max_bytes: Some(256),
            max_request_bytes: None,
            every_steps: None,
            on_change: false,
            bootstrap: false,
            valid_for_ms: None,
            timeout_ms: None,
            cancel_grace_ms: None,
        };
        let read = OutputField {
            family: None,
            name: "private_projection_method",
            kind: OutputKind::Read,
            port: Some("read"),
            port_signature: Some(crate::contracts::MethodSignature::new(
                "fixture.Service",
                "Read",
                "read",
                crate::contracts::MethodShape::Call,
                "fixture.Request",
                "fixture.Response",
                false,
                None,
                &[],
            )),
            input: None,
            project: Some("private_project_fn"),
            max_request_bytes: Some(128),
            ..reply
        };
        let local = InputField {
            name: "private_operation",
            kind: InputKind::Operation,
            port: None,
            port_signature: None,
            ..input
        };
        let bytes = runtime_record(
            RuntimeSpec::from_millis(20, 100, 1000),
            "null",
            "[]",
            &[input, local],
            &[reply],
            &[read],
        );
        let json = &bytes.as_bytes()[12..];
        assert!(!std::str::from_utf8(json).unwrap().contains("private_"));
        let record: crate::artifact::RuntimeRecord = serde_json::from_slice(json).unwrap();
        let crate::artifact::RuntimeRecord::V0 {
            inputs, outputs, ..
        } = record;
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].name, "command");
        assert_eq!(inputs[0].response_max_bytes, Some(256));
        assert_eq!(inputs[0].response_max_items, Some(2));
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].name, "read");
        let value: serde_json::Value = serde_json::from_slice(json).unwrap();
        let output = value["outputs"][0].as_object().unwrap();
        for private in ["role", "input", "project", "on_change", "cancel_grace_ms"] {
            assert!(!output.contains_key(private), "private field {private}");
        }
        assert!(value.get("transient_outputs").is_none());
        assert!(value.get("service_outputs").is_none());
    }
}
