//! Receiver reservations from admitted runtime periods and provider cadence.
use super::*;

pub(super) fn validate_controlled_capacity(
    instances: &[RuntimeInstance],
    artifacts: &BTreeMap<String, ArtifactRuntime>,
    connections: &BTreeMap<String, serde_json::Value>,
    observation_providers: &BTreeMap<
        (String, String),
        super::super::bundle::SourceSimulationProvider,
    >,
    scenario_program: Option<&Program>,
    quantum_ns: u64,
) -> Result<()> {
    let periods = instances
        .iter()
        .map(|runtime| (runtime.instance.as_str(), runtime.period_ns))
        .collect::<BTreeMap<_, _>>();
    for (consumer, source_values) in connections {
        let (consumer_instance, consumer_port) = consumer
            .split_once('.')
            .with_context(|| format!("connection consumer `{consumer}` has no port separator"))?;
        let consumer_artifact = artifacts
            .get(consumer_instance)
            .with_context(|| format!("connection consumer `{consumer}` has no runtime artifact"))?;
        let Some(input) = consumer_artifact.inputs.iter().find(|input| {
            input.name == consumer_port || input.port.as_deref() == Some(consumer_port)
        }) else {
            // The source compiler may retain a connection for an operation
            // field, which has no receiver queue reservation.
            continue;
        };
        if matches!(
            input.role.as_str(),
            "operation_result" | "call_completions" | "" | "call_result" | "call_target"
        ) {
            // Keyed exchanges enforce the caller's response-byte bound and
            // correlation lifecycle. The target Commands batch bounds requests
            // at its own receiver; it is not a publication batch to every caller.
            continue;
        }
        let consumer_period = *periods
            .get(consumer_instance)
            .with_context(|| format!("consumer `{consumer_instance}` has no admitted period"))?;
        let consumer_stride = consumer_period
            .checked_div(quantum_ns)
            .filter(|stride| *stride > 0)
            .with_context(|| {
                format!("consumer `{consumer_instance}` period is not quantum-aligned")
            })?;
        let replaceable = matches!(input.role.as_str(), "observation_latest" | "leased_value");
        let mut required_items = 0_u64;
        let mut required_bytes = 0_u64;
        for source in connection_source_values(consumer, source_values)? {
            let (source_instance, source_port) = source
                .split_once('.')
                .with_context(|| format!("connection source `{source}` has no port separator"))?;
            if source_instance == "scenario"
                && let Some(program) = scenario_program
            {
                let maximum = program
                    .steps()
                    .iter()
                    .filter_map(|step| match &step.action {
                        ScenarioAction::Setpoint {
                            consumer_signature,
                            encoded_payload,
                            ..
                        } if consumer_signature.name == source_port => {
                            u64::try_from(encoded_payload.len()).ok()
                        }
                        ScenarioAction::Withdraw {
                            producer_signature, ..
                        } if producer_signature.name == source_port => Some(0),
                        _ => None,
                    })
                    .max()
                    .with_context(|| {
                        format!("scenario source `{source}` has no matching program action")
                    })?;
                required_items = required_items.max(1);
                required_bytes = required_bytes.max(maximum.max(1));
                continue;
            }
            if let Some(provider) =
                observation_providers.get(&(source_instance.to_owned(), source_port.to_owned()))
            {
                // The public protocol enforces the declared rational source cadence.
                // Include one endpoint/initial-capture slot without pretending every
                // native quantum contains another source publication.
                let items = if replaceable {
                    1
                } else {
                    maximum_observations(consumer_period, quantum_ns, provider.rate_microhertz)?
                };
                let bytes = u64::from(provider.max_message_bytes).saturating_mul(items);
                if replaceable {
                    required_items = required_items.max(items);
                    required_bytes = required_bytes.max(bytes);
                } else {
                    required_items = required_items.saturating_add(items);
                    required_bytes = required_bytes.saturating_add(bytes);
                }
                continue;
            }
            let source_period = *periods
                .get(source_instance)
                .with_context(|| format!("source `{source_instance}` has no admitted period"))?;
            let source_stride = source_period
                .checked_div(quantum_ns)
                .filter(|stride| *stride > 0)
                .with_context(|| {
                    format!("source `{source_instance}` period is not quantum-aligned")
                })?;
            let source_artifact = artifacts
                .get(source_instance)
                .with_context(|| format!("source `{source}` has no runtime artifact"))?;
            let source_input = source_artifact.inputs.iter().find(|candidate| {
                candidate.port.as_deref() == Some(source_port) && candidate.role == "call_ingress"
            });
            let source_output = source_artifact
                .transient_outputs
                .iter()
                .chain(source_artifact.service_outputs.iter())
                .find(|output| output.port.as_deref() == Some(source_port));
            let (batch_items, batch_bytes, every_steps, bootstrap) =
                if let Some(input) = source_input {
                    // A Commands endpoint is a receiver queue on the target
                    // runtime.  Each connected caller can contribute at most one
                    // activation per due invocation, bounded by the target input.
                    (
                        input.max_items.unwrap_or(1),
                        input.max_bytes.unwrap_or(1),
                        1,
                        false,
                    )
                } else if let Some(output) = source_output {
                    (
                        output.max_items.unwrap_or(1),
                        output.max_bytes.or(output.max_request_bytes).unwrap_or(1),
                        output.every_steps.unwrap_or(1),
                        output.bootstrap,
                    )
                } else {
                    (1, 1, 1, false)
                };
            if every_steps == 0 {
                bail!("output `{source}` has a zero every_steps cadence");
            }
            let source_invocations = consumer_stride
                .checked_add(source_stride.saturating_sub(1))
                .and_then(|value| value.checked_div(source_stride))
                .unwrap_or(u64::MAX)
                .max(1);
            let emitted_batches = source_invocations
                .checked_add(every_steps.saturating_sub(1))
                .and_then(|value| value.checked_div(every_steps))
                .unwrap_or(u64::MAX)
                .saturating_add(u64::from(bootstrap));
            let route_items = batch_items.saturating_mul(emitted_batches);
            let route_bytes = batch_bytes.saturating_mul(emitted_batches);
            if replaceable {
                required_items = required_items.max(route_items.min(1));
                required_bytes = required_bytes.max(route_bytes.min(batch_bytes));
            } else {
                required_items = required_items.saturating_add(route_items);
                required_bytes = required_bytes.saturating_add(route_bytes);
            }
        }
        let available_items = input.max_items.unwrap_or(required_items.max(1));
        let available_bytes = input.max_bytes.unwrap_or(required_bytes.max(1));
        if required_items > available_items || required_bytes > available_bytes {
            bail!(
                "controlled receiver `{consumer}` capacity is insufficient: requires {required_items} items/{required_bytes} bytes but reserves {available_items} items/{available_bytes} bytes"
            );
        }
    }
    Ok(())
}

fn maximum_observations(period_ns: u64, quantum_ns: u64, rate_microhertz: u64) -> Result<u64> {
    if quantum_ns == 0 || period_ns == 0 || rate_microhertz == 0 {
        bail!("observation capacity requires positive period, quantum, and source rate");
    }
    let captures = (u128::from(period_ns) * u128::from(rate_microhertz))
        .div_ceil(1_000_000_000_000_000)
        .saturating_add(1);
    let quantum_ceiling = u128::from(period_ns / quantum_ns).saturating_add(1);
    u64::try_from(captures.min(quantum_ceiling)).context("observation queue reservation overflow")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_response_reservation_is_independent_of_target_command_batch() {
        for kind in ["request", "read"] {
            let caller: ArtifactRuntime = serde_json::from_value(serde_json::json!({
                "inputs": [{"name": "emergency", "kind": kind, "max_bytes": 1024}]
            }))
            .unwrap();
            let artifacts = BTreeMap::from([("brain".to_owned(), caller)]);
            let connections = BTreeMap::from([(
                "brain.emergency".to_owned(),
                serde_json::json!("motion.emergency"),
            )]);
            validate_controlled_capacity(
                &[],
                &artifacts,
                &connections,
                &BTreeMap::new(),
                None,
                2_000_000,
            )
            .expect("keyed completions are bounded independently of publication cadence");
        }
    }

    #[test]
    fn source_rate_bounds_four_encoders_without_reserving_every_quantum() {
        assert_eq!(
            maximum_observations(20_000_000, 2_000_000, 50_000_000).unwrap(),
            2
        );
        assert_eq!(
            4 * maximum_observations(20_000_000, 2_000_000, 50_000_000).unwrap(),
            8
        );
        assert!(maximum_observations(20_000_000, 2_000_000, 0).is_err());
    }

    #[test]
    fn rational_source_phases_never_exceed_the_closed_interval_reservation() {
        let quantum = 2_000_000_u64;
        for rate in [
            1_u64,
            30_000_000,
            50_000_000,
            59_940_000,
            499_000_000,
            500_000_000,
        ] {
            for stride in [1_u64, 10, 50] {
                let capacity = maximum_observations(stride * quantum, quantum, rate).unwrap();
                for start in 0_u64..500 {
                    let count = (start..=start + stride)
                        .filter(|boundary| {
                            *boundary == 0
                                || u128::from(*boundary) * u128::from(rate) * u128::from(quantum)
                                    / 1_000_000_000_000_000
                                    > u128::from(boundary - 1)
                                        * u128::from(rate)
                                        * u128::from(quantum)
                                        / 1_000_000_000_000_000
                        })
                        .count() as u64;
                    assert!(
                        count <= capacity,
                        "rate={rate} stride={stride} start={start} count={count} capacity={capacity}"
                    );
                }
            }
        }
    }
}
