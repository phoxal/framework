//! Admission of authored connections against executable contracts.

use super::{ArtifactContract, Error, InputKind, PortKind};
use crate::document::{PortReference, RobotDocument};
use std::collections::BTreeMap;

pub fn validate_connected_endpoints(
    document: &RobotDocument,
    contracts: &BTreeMap<String, ArtifactContract>,
) -> Result<(), Error> {
    for (instance, contract) in contracts {
        for input in &contract.runtime.inputs {
            if matches!(input.kind, InputKind::Commands | InputKind::Operation) {
                continue;
            }
            let consumer = format!("{instance}.{}", input.name);
            if !document.connections.contains_key(&consumer) {
                return Err(Error::InvalidConnection {
                    consumer,
                    producer: String::new(),
                    message: "required runtime input has no authored connection".into(),
                });
            }
        }
    }
    for (consumer_text, sources) in &document.connections {
        let consumer =
            PortReference::parse(consumer_text).map_err(|error| Error::InvalidConnection {
                consumer: consumer_text.clone(),
                producer: String::new(),
                message: error.to_string(),
            })?;
        let consumer_contract =
            contracts
                .get(&consumer.instance)
                .ok_or_else(|| Error::InvalidConnection {
                    consumer: consumer_text.clone(),
                    producer: String::new(),
                    message: "consumer has no executable runtime artifact".into(),
                })?;
        let input = consumer_contract
            .runtime
            .inputs
            .iter()
            .find(|input| input.name == consumer.port)
            .ok_or_else(|| Error::InvalidConnection {
                consumer: consumer_text.clone(),
                producer: String::new(),
                message: "consumer input is absent from the runtime artifact".to_owned(),
            })?;
        if sources.as_slice().is_empty()
            || (matches!(
                input.kind,
                InputKind::Latest | InputKind::Setpoint | InputKind::Read | InputKind::Request
            ) && sources.as_slice().len() != 1)
        {
            return Err(Error::InvalidConnection {
                consumer: consumer_text.clone(),
                producer: String::new(),
                message: "input requires exactly one publisher or request target".into(),
            });
        }
        for producer_text in sources.as_slice() {
            let producer =
                PortReference::parse(producer_text).map_err(|error| Error::InvalidConnection {
                    consumer: consumer_text.clone(),
                    producer: producer_text.clone(),
                    message: error.to_string(),
                })?;
            let Some(producer_contract) = contracts.get(&producer.instance) else {
                if document.robot.components.contains_key(&producer.instance) {
                    // Simulation providers are admitted from the selected
                    // component contracts during native bundle preparation.
                    continue;
                }
                return Err(Error::InvalidConnection {
                    consumer: consumer_text.clone(),
                    producer: producer_text.clone(),
                    message: "producer has no executable runtime artifact".into(),
                });
            };
            let signature = if input.kind == InputKind::Request {
                producer_contract
                    .runtime
                    .inputs
                    .iter()
                    .find(|input| {
                        input.kind == InputKind::Commands
                            && input.port.as_deref() == Some(producer.port.as_str())
                    })
                    .and_then(|input| input.signature.as_ref())
            } else {
                producer_contract
                    .runtime
                    .service_outputs
                    .iter()
                    .chain(producer_contract.runtime.transient_outputs.iter())
                    .find(|output| output.port.as_deref() == Some(producer.port.as_str()))
                    .and_then(|output| output.signature.as_ref())
            }
            .ok_or_else(|| Error::InvalidConnection {
                consumer: consumer_text.clone(),
                producer: producer_text.clone(),
                message:
                    "producer port is absent or has no complete signature in its runtime artifact"
                        .into(),
            })?;
            let expected_kind = input_kind_port(input.kind);
            if expected_kind != Some(signature.kind) {
                return Err(Error::InvalidConnection {
                    consumer: consumer_text.clone(),
                    producer: producer_text.clone(),
                    message: format!(
                        "consumer {:?} requires {:?}, producer supplies {:?}",
                        input.kind, expected_kind, signature.kind
                    ),
                });
            }
            if input
                .request_fqn
                .as_ref()
                .is_some_and(|request| request != &signature.request)
                || input
                    .response_fqn
                    .as_ref()
                    .is_some_and(|response| response != &signature.response)
            {
                return Err(Error::InvalidConnection { consumer: consumer_text.clone(), producer: producer_text.clone(),
                    message: "generated Protobuf request or response type differs from the consumer input".into() });
            }
            if let Some(input_signature) = &input.signature
                && input_signature != signature
            {
                return Err(Error::InvalidConnection {
                    consumer: consumer_text.clone(),
                    producer: producer_text.clone(),
                    message: "request, response, service, or method identity differs".to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn input_kind_port(kind: InputKind) -> Option<PortKind> {
    match kind {
        InputKind::Latest => Some(PortKind::State),
        InputKind::Samples => Some(PortKind::Sample),
        InputKind::Events => Some(PortKind::Event),
        InputKind::Setpoint => Some(PortKind::Setpoint),
        InputKind::Stream => Some(PortKind::Stream),
        InputKind::Commands | InputKind::Request => Some(PortKind::Commands),
        InputKind::Read => Some(PortKind::Read),
        InputKind::Operation => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn contract(inputs: serde_json::Value, outputs: serde_json::Value) -> ArtifactContract {
        ArtifactContract {
            runtime: serde_json::from_value(json!({
                "schema": "phoxal/artifact/v0", "record": "runtime", "period_ms": 20,
                "timeout_ms": 100, "init_timeout_ms": 1000, "config_schema": {},
                "inputs": inputs, "transient_outputs": [], "service_outputs": outputs,
            }))
            .unwrap(),
            descriptors: Vec::new(),
        }
    }
    fn document(connections: serde_json::Value) -> RobotDocument {
        serde_json::from_value(
            json!({"robot": {"id": "test"}, "services": {}, "connections": connections}),
        )
        .unwrap()
    }

    #[test]
    fn missing_required_connection_is_rejected_even_when_the_graph_is_empty() {
        let contracts = BTreeMap::from([(
            "motion".into(),
            contract(json!([{"name":"safety", "kind":"latest"}]), json!([])),
        )]);
        assert!(validate_connected_endpoints(&document(json!({})), &contracts).is_err());
    }

    #[test]
    fn request_targets_the_server_command_input() {
        let signature = json!({"name":"commands", "service":"fixture.Service", "method":"Commands", "kind":"commands", "request":"fixture.Request", "response":"fixture.Response"});
        let contracts = BTreeMap::from([
            (
                "client".into(),
                contract(json!([{"name":"request", "kind":"request"}]), json!([])),
            ),
            (
                "server".into(),
                contract(
                    json!([{"name":"incoming", "kind":"commands", "port":"commands", "signature":signature}]),
                    json!([]),
                ),
            ),
        ]);
        validate_connected_endpoints(
            &document(json!({"client.request":"server.commands"})),
            &contracts,
        )
        .unwrap();
    }

    #[test]
    fn latest_rejects_multiple_publishers_instead_of_competing_for_one_slot() {
        let contracts = BTreeMap::from([(
            "consumer".into(),
            contract(json!([{"name":"state", "kind":"latest"}]), json!([])),
        )]);
        assert!(
            validate_connected_endpoints(
                &document(json!({"consumer.state":["first.state","second.state"]})),
                &contracts
            )
            .is_err()
        );
    }
    #[test]
    fn generated_request_and_response_identity_are_checked_before_launch() {
        let signature = json!({"name":"commands", "service":"fixture.Service", "method":"Commands", "kind":"commands", "request":"fixture.Request", "response":"fixture.Response"});
        for (request, response) in [
            ("fixture.Other", "fixture.Response"),
            ("fixture.Request", "fixture.Other"),
        ] {
            let contracts = BTreeMap::from([
                (
                    "client".into(),
                    contract(
                        json!([{"name":"request", "kind":"request", "request_fqn": request, "response_fqn": response}]),
                        json!([]),
                    ),
                ),
                (
                    "server".into(),
                    contract(
                        json!([{"name":"incoming", "kind":"commands", "port":"commands", "signature":signature}]),
                        json!([]),
                    ),
                ),
            ]);
            let error = validate_connected_endpoints(
                &document(json!({"client.request":"server.commands"})),
                &contracts,
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Protobuf request or response type differs")
            );
        }
    }
}
