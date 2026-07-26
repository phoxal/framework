//! `#[derive(phoxal::Config)]` schema tests: the const schema the derive emits
//! must match a `schemars` oracle over the supported surface, and must accept
//! exactly the documents `serde` accepts.
//!
//! These exercise macro output directly, without a bus or a runner.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};

use crate::participant::ParticipantConfig;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[derive(Debug, serde::Deserialize, JsonSchema, phoxal::Config)]
struct FlatScalars {
    name: String,
    enabled: bool,
    count: i32,
    sequence: u64,
    ratio: f64,
}

#[derive(Debug, serde::Deserialize, JsonSchema, phoxal::Config)]
#[serde(
    rename = "RenamedConfig",
    rename_all = "camelCase",
    deny_unknown_fields
)]
struct SerdeSurface {
    explicit_name: String,
    #[serde(rename = "retries")]
    retry_count: u32,
    #[serde(default)]
    start_paused: bool,
    #[serde(default = "default_limit")]
    sample_limit: u16,
}

fn default_limit() -> u16 {
    10
}

#[derive(Debug, serde::Deserialize, JsonSchema, phoxal::Config)]
#[serde(deny_unknown_fields)]
struct Collections {
    labels: Vec<String>,
    maybe_count: Option<i64>,
    ordered: BTreeMap<String, u32>,
    hashed: HashMap<String, bool>,
}

#[derive(Debug, serde::Deserialize, JsonSchema, phoxal::Config)]
#[serde(deny_unknown_fields)]
struct Nested {
    id: String,
    options: SerdeSurface,
    batches: Vec<Collections>,
}

#[test]
fn const_schema_matches_schemars_oracle_for_supported_surface() {
    assert_oracle::<FlatScalars>();
    assert_oracle::<SerdeSurface>();
    assert_oracle::<Collections>();
    assert_oracle::<Nested>();
}

#[test]
fn schema_validation_and_serde_deserialization_agree() {
    assert_schema_and_serde_agree::<SerdeSurface>(&[
        json!({"explicitName":"motor","retries":3}),
        json!({"explicitName":"motor","retries":3,"startPaused":true,"sampleLimit":8}),
        json!({"explicitName":"motor"}),
        json!({"explicitName":"motor","retries":3,"unknown":true}),
        json!({"explicitName":7,"retries":3}),
        Value::Null,
    ]);

    assert_schema_and_serde_agree::<Nested>(&[
        json!({
            "id":"robot-a",
            "options":{"explicitName":"motor","retries":3},
            "batches":[{
                "labels":["front","left"],
                "maybe_count":null,
                "ordered":{"one":1},
                "hashed":{"ready":true}
            }]
        }),
        json!({"id":"robot-a","options":{},"batches":[]}),
        json!({"id":"robot-a","options":{"explicitName":"motor","retries":3},"batches":{}}),
        json!({"id":"robot-a","options":{"explicitName":"motor","retries":3},"batches":[],"extra":0}),
    ]);
}

fn assert_oracle<T: ParticipantConfig + JsonSchema>() {
    let actual: Value = serde_json::from_str(T::SCHEMA_JSON).expect("derive emits valid JSON");
    let oracle = serde_json::to_value(schemars::schema_for!(T)).expect("serialize oracle schema");
    assert_eq!(
        normalize_schema(actual.clone()),
        normalize_schema(oracle.clone()),
        "const schema differs from schemars oracle\nconst: {actual}\noracle: {oracle}"
    );
}

fn assert_schema_and_serde_agree<T: ParticipantConfig + DeserializeOwned>(values: &[Value]) {
    let schema: Value = serde_json::from_str(T::SCHEMA_JSON).expect("derive emits valid JSON");
    let validator = jsonschema::validator_for(&schema).expect("derive emits a valid schema");
    for value in values {
        let schema_accepts = validator.is_valid(value);
        let serde_accepts = serde_json::from_value::<T>(value.clone()).is_ok();
        assert_eq!(
            schema_accepts, serde_accepts,
            "schema and serde disagree for {value}: schema={schema_accepts}, serde={serde_accepts}"
        );
    }
}

fn normalize_schema(mut schema: Value) -> Value {
    let definitions = schema
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    normalize_node(&mut schema, &definitions);
    schema
}

fn normalize_node(node: &mut Value, definitions: &serde_json::Map<String, Value>) {
    if let Some(reference) = node
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
    {
        *node = definitions
            .get(reference)
            .unwrap_or_else(|| panic!("missing schemars definition {reference}"))
            .clone();
        normalize_node(node, definitions);
        return;
    }

    if let Value::Object(object) = node
        && let Some(Value::Array(types)) = object.get("type")
        && types.iter().any(|ty| ty == "null")
    {
        let mut base = object.clone();
        base.remove("type");
        let mut alternatives = Vec::new();
        for ty in types {
            let mut alternative = if ty == "null" {
                serde_json::Map::new()
            } else {
                base.clone()
            };
            alternative.insert("type".to_string(), ty.clone());
            alternatives.push(Value::Object(alternative));
        }
        *node = json!({"anyOf": alternatives});
    }

    match node {
        Value::Object(object) => {
            for annotation in [
                "$schema",
                "$defs",
                "title",
                "description",
                "default",
                "examples",
                "deprecated",
                "readOnly",
                "writeOnly",
            ] {
                object.remove(annotation);
            }
            for value in object.values_mut() {
                normalize_node(value, definitions);
            }
            if let Some(Value::Array(required)) = object.get_mut("required") {
                required.sort_by_key(Value::to_string);
            }
            if let Some(Value::Array(any_of)) = object.get_mut("anyOf") {
                any_of.sort_by_key(Value::to_string);
            }
        }
        Value::Array(values) => {
            for value in values {
                normalize_node(value, definitions);
            }
        }
        _ => {}
    }
}
