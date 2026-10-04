//! Robot-owned observation conversions attached to the normal endpoint contract.
//!
//! API generation owns discovery and embeds the actual mapping in the brain
//! contract. The tool consumes that compiled mapping without recreating generator
//! choices. There is no secondary runtime or executable role.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Deserialize;

/// One observation conversion executed in the brain's ordinary invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConversionEdge {
    /// Authored consumer endpoint.
    pub consumer: String,
    /// Authored producer endpoint.
    pub producer: String,
    /// Producer's canonical payload identity.
    pub source_type: String,
    /// Consumer's canonical payload identity.
    pub destination_type: String,
    /// Consumer's admitted capture-age bound.
    pub max_age_ms: Option<u64>,
    /// Source byte bound.
    pub input_max_bytes: u64,
    /// Converted byte bound.
    pub output_max_bytes: u64,
}

impl ConversionEdge {
    /// Input field added to the brain's normal endpoint declaration.
    pub fn input_name(index: usize) -> String {
        format!("phoxal_conversion_in_{index}")
    }

    /// Output field added to the brain's normal endpoint declaration.
    pub fn output_name(index: usize) -> String {
        format!("phoxal_conversion_out_{index}")
    }
}

#[derive(Deserialize)]
struct Runtime {
    inputs: Vec<Input>,
    outputs: Vec<Output>,
}

#[derive(Deserialize)]
struct Input {
    name: String,
    delivery: String,
    response_fqn: Option<String>,
    max_age_ms: Option<u64>,
    max_bytes: Option<u64>,
}

#[derive(Deserialize)]
struct Output {
    signature: Option<Signature>,
    max_bytes: Option<u64>,
}

#[derive(Deserialize)]
struct Signature {
    endpoint: String,
    shape: String,
    request: String,
    response: String,
    retained_latest: bool,
    lease_valid_for_ms: Option<u64>,
}

/// Discovers conversions between selected providers from their current contracts.
///
/// Brain-owned inputs use their authored canonical payload type and ordinary
/// handlers. They need no generated copy of the brain's own Rust definitions.
pub(crate) fn discover_conversions(
    contracts: &BTreeMap<String, serde_json::Value>,
    connections: &BTreeMap<String, String>,
) -> Result<Vec<ConversionEdge>, String> {
    let contracts = contracts
        .iter()
        .map(|(name, contract)| {
            serde_json::from_value::<Runtime>(contract.clone())
                .map(|contract| (name.as_str(), contract))
                .map_err(|error| format!("invalid prepared contract for {name}: {error}"))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let mut edges = Vec::new();
    for (consumer, producer) in connections {
        let (Some((consumer_instance, consumer_port)), Some((producer_instance, producer_port))) =
            (consumer.split_once('.'), producer.split_once('.'))
        else {
            continue;
        };
        let (Some(consumer_contract), Some(producer_contract)) = (
            contracts.get(consumer_instance),
            contracts.get(producer_instance),
        ) else {
            continue;
        };
        let Some(input) = consumer_contract
            .inputs
            .iter()
            .find(|input| input.name == consumer_port)
        else {
            continue;
        };
        let Some(output) = producer_contract.outputs.iter().find(|output| {
            output
                .signature
                .as_ref()
                .is_some_and(|signature| signature.endpoint == producer_port)
        }) else {
            continue;
        };
        let Some(signature) = &output.signature else {
            continue;
        };
        let Some(destination) = &input.response_fqn else {
            continue;
        };
        if input.delivery != "observation_latest"
            || signature.shape != "observation"
            || *destination == signature.response
        {
            continue;
        }
        if !signature.retained_latest
            || signature.request != "google.protobuf.Empty"
            || signature.lease_valid_for_ms.is_some()
        {
            return Err(format!(
                "{consumer} <- {producer}: conversion requires a retained, unleased observation"
            ));
        }
        edges.push(ConversionEdge {
            consumer: consumer.clone(),
            producer: producer.clone(),
            source_type: signature.response.clone(),
            destination_type: destination.clone(),
            max_age_ms: Some(input.max_age_ms.ok_or_else(|| {
                format!("{consumer}: conversion requires a declared capture-age bound")
            })?),
            input_max_bytes: output.max_bytes.ok_or_else(|| {
                format!("{producer}: conversion requires a compiled output byte bound")
            })?,
            output_max_bytes: input.max_bytes.ok_or_else(|| {
                format!("{consumer}: conversion requires a compiled input byte bound")
            })?,
        });
    }
    Ok(edges)
}

/// Emits the conversion attachment inside the existing generated API product.
pub(crate) fn render_attachment(edges: &[ConversionEdge]) -> String {
    let mut fields = String::from("{");
    let mut step = String::from("{");
    for (index, edge) in edges.iter().enumerate() {
        let input = ConversionEdge::input_name(index);
        let output = ConversionEdge::output_name(index);
        let source_type = type_path(&edge.source_type);
        let destination = type_path(&edge.destination_type);
        let age = edge
            .max_age_ms
            .map_or_else(String::new, |age| format!("max_age_ms = {age}, "));
        let _ = writeln!(
            fields,
            "#[phoxal::input({age}max_bytes = {})] {input}: ::phoxal::contracts::Latest<{source_type}>,",
            edge.input_max_bytes
        );
        let _ = writeln!(
            fields,
            "#[phoxal::output(stamped, max_bytes = {})] {output}: ::phoxal::contracts::Latest<{destination}>,",
            edge.output_max_bytes
        );
        let label = format!("{} <- {}: {{error:?}}", edge.consumer, edge.producer);
        let _ = writeln!(
            step,
            "if inputs.{input}.is_fresh_at(ctx.now(), {:?}) && let Some(sample) = inputs.{input}.sample() {{\nlet converted: {destination} = sample.payload().clone().try_into().map_err(|error| ::phoxal::anyhow!({label:?}))?;\noutputs.{output} = Some(::phoxal::runtime::Sample::new(converted, sample.stamp().clone()));\n}}",
            edge.max_age_ms
        );
    }
    fields.push('}');
    step.push('}');
    let routes = edges
        .iter()
        .enumerate()
        .map(|(index, edge)| {
            serde_json::json!({
                "producer": edge.producer, "consumer": edge.consumer,
                "input_endpoint": ConversionEdge::input_name(index),
                "output_endpoint": ConversionEdge::output_name(index),
            })
        })
        .collect::<Vec<_>>();
    let routes = serde_json::Value::Array(routes).to_string();
    format!(
        "#[allow(dead_code)]\nconst PHOXAL_CONVERSION_ENDPOINTS: &str = {fields:?};\n#[allow(dead_code)]\nconst PHOXAL_CONVERSION_STEP: &str = {step:?};\n#[allow(dead_code)]\nconst PHOXAL_CONVERSION_ROUTES: &str = {routes:?};\n"
    )
}

fn type_path(identity: &str) -> String {
    crate::sdk_type_path(identity)
        .unwrap_or_else(|| format!("crate::api::types::{}", identity.replace('.', "::")))
}
