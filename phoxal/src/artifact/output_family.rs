//! Pure resolution of bounded, configuration-owned output ports.
//!
//! Executable records retain templates. Each instance resolves those templates
//! against its admitted configuration without executing application code.

use super::{MethodShape, OutputRecord, RuntimeRecord};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One typed leased output per key in a configuration object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputFamily {
    /// JSON pointer selecting the object whose keys are logical port names.
    pub config_pointer: String,
    /// Literal suffix appended to every logical name.
    pub suffix: String,
    /// Maximum number of resolved ports, including disconnected ports.
    pub max_ports: u64,
}

impl OutputFamily {
    /// Validates the bounded template independently of configuration.
    pub fn validate(&self, output: &OutputRecord) -> Result<(), String> {
        if !self.config_pointer.starts_with('/')
            || self.config_pointer.len() > 256
            || self.max_ports == 0
            || self.max_ports > 64
        {
            return Err(
                "output family requires a configuration pointer and 1-64 port bound".into(),
            );
        }
        super::bundle::validate_segment(&self.suffix, "output family suffix")?;
        let signature = output
            .signature
            .as_ref()
            .ok_or("output family has no typed signature")?;
        if signature.shape != MethodShape::Observation
            || signature.request != "google.protobuf.Empty"
            || !signature.retained_latest
            || signature.lease_valid_for_ms.is_none_or(|lease| lease == 0)
            || output.max_bytes.is_none_or(|bytes| bytes == 0)
            || output.max_items.is_some_and(|items| items != 1)
        {
            return Err("output family must be a bounded canonical leased observation".into());
        }
        Ok(())
    }
}

impl RuntimeRecord {
    /// Resolves the complete output contract for one configuration.
    ///
    /// The schema, template and every generated port are validated before any
    /// result escapes. Fixed ports and other families share one collision set.
    pub fn resolve_outputs(&self, config: &serde_json::Value) -> Result<Self, String> {
        super::bundle::validate_runtime_record(self, "output template")?;
        let Self::V0 {
            config_schema,
            outputs,
            ..
        } = self;
        if outputs
            .iter()
            .any(|output| output.family_template.is_some())
        {
            return Err("source executable contract cannot contain resolved family members".into());
        }
        let validator = jsonschema::validator_for(config_schema)
            .map_err(|error| format!("invalid configuration schema: {error}"))?;
        if let Some(error) = validator.iter_errors(config).next() {
            return Err(format!(
                "invalid output configuration: {error} at {}",
                error.instance_path()
            ));
        }
        let mut resolved = Vec::new();
        let mut ports = BTreeSet::new();
        for output in outputs {
            if let Some(family) = &output.family {
                family.validate(output)?;
                let names = config
                    .pointer(&family.config_pointer)
                    .and_then(serde_json::Value::as_object)
                    .ok_or_else(|| {
                        format!(
                            "output family {} must select a configuration object",
                            family.config_pointer
                        )
                    })?;
                if names.is_empty() || names.len() as u64 > family.max_ports {
                    return Err(format!(
                        "output family {} requires 1-{} ports",
                        output.name, family.max_ports
                    ));
                }
                for name in names.keys() {
                    super::bundle::validate_segment(name, "logical output name")?;
                    let port = format!("{name}{}", family.suffix);
                    super::bundle::validate_segment(&port, "resolved output port")?;
                    let mut member = output.clone();
                    member.family = None;
                    member.family_template = output.port.clone();
                    member.name = port.clone();
                    member.port = Some(port.clone());
                    let signature = member
                        .signature
                        .as_mut()
                        .ok_or("output family has no typed signature")?;
                    signature.endpoint = port.clone();
                    signature.method = port;
                    insert(&mut resolved, &mut ports, member)?;
                }
            } else {
                insert(&mut resolved, &mut ports, output.clone())?;
            }
        }
        let mut runtime = self.clone();
        let Self::V0 { outputs, .. } = &mut runtime;
        *outputs = resolved;
        super::bundle::validate_runtime_record(&runtime, "resolved output contract")?;
        Ok(runtime)
    }
}

fn insert(
    outputs: &mut Vec<OutputRecord>,
    ports: &mut BTreeSet<String>,
    output: OutputRecord,
) -> Result<(), String> {
    if !ports.insert(output.port.clone().ok_or("output has no port")?) {
        return Err(format!("resolved output port {} collides", output.name));
    }
    outputs.push(output);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn template() -> RuntimeRecord {
        serde_json::from_value(json!({
            "schema":"phoxal/artifact/v0", "record":"runtime",
            "period_ms":20,"timeout_ms":100,"init_timeout_ms":1000,
            "config_schema":{"type":"object","required":["wheels"],
                "properties":{"wheels":{"type":"object"}},"additionalProperties":false},
            "inputs":[], "outputs":[{
                "name":"wheel_projection", "port":"wheels",
                "family":{"config_pointer":"/wheels","suffix":"_actuator","max_ports":8},
                "signature":{"endpoint":"wheels","service":"example.Control",
                    "method":"wheels","shape":"observation","request":"google.protobuf.Empty",
                    "response":"example.Control","retained_latest":true,"lease_valid_for_ms":100},
                "max_items":null,"max_bytes":256,"max_request_bytes":null,
                "every_steps":null,"bootstrap":false,"timeout_ms":null
            }]
        }))
        .expect("typed template")
    }

    fn ports(runtime: &RuntimeRecord) -> Vec<&str> {
        let RuntimeRecord::V0 { outputs, .. } = runtime;
        outputs
            .iter()
            .map(|output| output.port.as_deref().unwrap())
            .collect()
    }

    #[test]
    fn shared_template_resolves_independent_configurations_without_mutation() {
        let source = template();
        let left = source
            .resolve_outputs(&json!({"wheels":{"front_left":{},"rear_left":{}}}))
            .unwrap();
        let right = source
            .resolve_outputs(&json!({"wheels":{"right":{}}}))
            .unwrap();
        assert_eq!(ports(&left), ["front_left_actuator", "rear_left_actuator"]);
        assert_eq!(ports(&right), ["right_actuator"]);
        assert_eq!(ports(&source), ["wheels"]);
        let RuntimeRecord::V0 { outputs, .. } = left;
        assert!(outputs.iter().all(|output| output.family.is_none()));
        assert!(
            outputs
                .iter()
                .all(|output| output.family_template.as_deref() == Some("wheels"))
        );
        assert!(
            outputs
                .iter()
                .all(|output| output.signature.as_ref().unwrap().lease_valid_for_ms == Some(100))
        );
    }

    #[test]
    fn refuses_unsafe_empty_oversized_and_colliding_membership() {
        let source = template();
        for config in [
            json!({"wheels":{}}),
            json!({"wheels":{"../motor":{}}}),
            json!({"wheels":{"Front":{}}}),
            json!({"wheels":[]}),
            json!({}),
        ] {
            assert!(source.resolve_outputs(&config).is_err(), "{config}");
        }
        let oversized: serde_json::Map<String, serde_json::Value> = (0..9)
            .map(|index| (format!("wheel{index}"), json!({})))
            .collect();
        assert!(
            source
                .resolve_outputs(&json!({"wheels":oversized}))
                .is_err()
        );
        let mut collision = source.clone();
        let RuntimeRecord::V0 { outputs, .. } = &mut collision;
        let mut fixed = outputs[0].clone();
        fixed.family = None;
        fixed.name = "front_actuator".into();
        fixed.port = Some(fixed.name.clone());
        fixed.signature.as_mut().unwrap().endpoint = fixed.name.clone();
        outputs.push(fixed);
        assert!(
            collision
                .resolve_outputs(&json!({"wheels":{"front":{}}}))
                .unwrap_err()
                .contains("collides")
        );
    }

    #[test]
    fn refuses_invalid_family_type_and_bounds() {
        for mutation in ["lease", "shape", "request", "bytes", "count"] {
            let mut runtime = template();
            let RuntimeRecord::V0 { outputs, .. } = &mut runtime;
            match mutation {
                "lease" => outputs[0].signature.as_mut().unwrap().lease_valid_for_ms = None,
                "shape" => outputs[0].signature.as_mut().unwrap().shape = MethodShape::Call,
                "request" => {
                    outputs[0].signature.as_mut().unwrap().request = "example.Control".into()
                }
                "bytes" => outputs[0].max_bytes = None,
                "count" => outputs[0].family.as_mut().unwrap().max_ports = 65,
                _ => unreachable!(),
            }
            assert!(
                runtime
                    .resolve_outputs(&json!({"wheels":{"front":{}}}))
                    .is_err(),
                "{mutation}"
            );
        }
    }
}
