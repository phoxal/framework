//! Command-owned native implementation selection for a common runnable build.
//! The graph remains authored; this selects native implementations for its drivers.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::bundle::{AdmittedBundle, BundleSimulation, EndpointReference, InstanceRole};
use super::{InputDelivery, MethodShape, RuntimeRecord};

/// Native facts bound to the exact admitted build manifest, never executable bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum SimulationContext {
    /// The current command-owned context.
    #[serde(rename = "phoxal/simulation-context/v0")]
    V0 {
        /// Identity of the immutable graph and compiled contracts being executed.
        manifest_sha256: String,
        /// Native model identity, cadence and implementation bindings.
        simulation: BundleSimulation,
    },
}

impl SimulationContext {
    /// Binds qualified native facts to one manifest.
    #[must_use]
    pub fn new(manifest: &[u8], simulation: BundleSimulation) -> Self {
        Self::V0 {
            manifest_sha256: format!("{:x}", Sha256::digest(manifest)),
            simulation,
        }
    }

    /// Validates complete native driver coverage without altering a connection.
    /// Only after success may an owner omit those physical executables from launch.
    pub fn admit(self, manifest: &[u8], build: &mut AdmittedBundle) -> Result<(), String> {
        let Self::V0 {
            manifest_sha256,
            simulation,
        } = self;
        if manifest_sha256 != format!("{:x}", Sha256::digest(manifest)) {
            return Err("native context references another build manifest".into());
        }
        super::bundle::validate_simulation(&simulation, &build.components)?;
        let mut expected = BTreeSet::new();
        for instance in build
            .instances
            .values()
            .filter(|i| i.role == InstanceRole::Driver)
        {
            if !build
                .components
                .get(&instance.id)
                .is_some_and(|component| component.driver)
            {
                return Err(format!(
                    "native driver {} has no component declaration",
                    instance.id
                ));
            }
            let RuntimeRecord::V0 {
                inputs, outputs, ..
            } = build
                .instance_runtime(&instance.id)
                .ok_or_else(|| format!("native driver {} has no compiled contract", instance.id))?;
            for output in outputs {
                let (Some(port), Some(signature)) = (&output.port, &output.signature) else {
                    continue;
                };
                if signature.shape != MethodShape::Observation {
                    return Err(format!(
                        "native driver {}.{port} is not an observation",
                        instance.id
                    ));
                }
                expected.insert((instance.id.clone(), port.clone()));
                let provider = simulation
                    .providers
                    .iter()
                    .find(|p| p.service_instance == instance.id && p.port == *port)
                    .ok_or_else(|| format!("native implementation omits {}.{port}", instance.id))?;
                if provider.shape != signature.shape
                    || provider.service_fqn != signature.service
                    || provider.method != signature.method
                    || provider.input_fqn != signature.request
                    || provider.payload_fqn != signature.response
                    || provider.lease_valid_for_ms != signature.lease_valid_for_ms
                    || provider.retained_latest != signature.retained_latest
                    || Some(u64::from(provider.max_message_bytes)) != output.max_bytes
                    || u64::from(provider.max_buffered_items) != output.max_items.unwrap_or(1)
                    || u128::from(provider.rate_microhertz) * u128::from(simulation.quantum_ns)
                        > 1_000_000_000_000_000
                    || provider.rate_microhertz == 0
                {
                    return Err(format!(
                        "native implementation disagrees with {}.{port}",
                        instance.id
                    ));
                }
            }
            for (consumer, sources) in &build.execution_connections {
                if consumer.instance != instance.id {
                    continue;
                }
                let input = inputs
                    .iter()
                    .find(|i| {
                        i.name == consumer.endpoint || i.port.as_deref() == Some(&consumer.endpoint)
                    })
                    .ok_or_else(|| format!("native input {consumer} has no compiled contract"))?;
                if input.delivery != InputDelivery::LeasedValue {
                    return Err(format!(
                        "native implementation cannot serve input {consumer}"
                    ));
                }
                for source in sources {
                    let binding = simulation
                        .actuation_bindings
                        .iter()
                        .find(|b| {
                            b.service_instance == source.instance && b.port == source.endpoint
                        })
                        .ok_or_else(|| {
                            format!(
                                "native implementation omits input route {source} -> {consumer}"
                            )
                        })?;
                    if input.request_fqn.as_deref() != Some(&binding.payload_fqn) {
                        return Err(format!(
                            "native actuation payload disagrees with {consumer}"
                        ));
                    }
                }
            }
        }
        let actual = simulation
            .providers
            .iter()
            .map(|p| (p.service_instance.clone(), p.port.clone()))
            .collect::<BTreeSet<_>>();
        if actual != expected {
            return Err("native providers do not exactly cover authored driver contracts".into());
        }
        let expected_actuators = actuator_routes(build)?;
        let actual_actuators = simulation
            .actuation_bindings
            .iter()
            .map(|binding| {
                (
                    EndpointReference {
                        instance: binding.service_instance.clone(),
                        endpoint: binding.port.clone(),
                    },
                    binding
                        .actuator_ids
                        .iter()
                        .cloned()
                        .collect::<BTreeSet<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        if actual_actuators != expected_actuators {
            return Err(
                "native actuator membership does not exactly follow authored driver input edges"
                    .into(),
            );
        }
        for binding in &simulation.actuation_bindings {
            let RuntimeRecord::V0 { outputs, .. } = build
                .instance_runtime(&binding.service_instance)
                .ok_or_else(|| {
                    format!(
                        "native actuator source {} is not authored",
                        binding.service_instance
                    )
                })?;
            let output = outputs
                .iter()
                .find(|o| o.port.as_deref() == Some(&binding.port))
                .ok_or_else(|| {
                    format!(
                        "native actuator source {}.{} has no served port",
                        binding.service_instance, binding.port
                    )
                })?;
            if !output.signature.as_ref().is_some_and(|signature| {
                signature.shape == MethodShape::Observation
                    && signature.lease_valid_for_ms.is_some_and(|lease| lease > 0)
                    && signature.response == binding.payload_fqn
            }) {
                return Err("native actuator source is not a canonical leased observation".into());
            }
        }
        build.simulation = Some(simulation);
        Ok(())
    }
}

/// Resolves native motor membership solely from authored component input edges.
/// Disconnected motors are never assigned to an arbitrary producer.
/// Compiled input/output contracts are already checked by common build admission.
pub fn actuator_routes(
    build: &AdmittedBundle,
) -> Result<BTreeMap<EndpointReference, BTreeSet<String>>, String> {
    use super::document::{ComponentDocument, NativeTargetKind};
    let mut routes = BTreeMap::<EndpointReference, BTreeSet<String>>::new();
    for (consumer, sources) in &build.execution_connections {
        let Some(instance) = build
            .instances
            .get(&consumer.instance)
            .filter(|instance| instance.role == InstanceRole::Driver)
        else {
            continue;
        };
        let component = build
            .components
            .get(&instance.id)
            .ok_or_else(|| format!("native driver {} has no component declaration", instance.id))?;
        let RuntimeRecord::V0 { inputs, .. } = build
            .instance_runtime(&instance.id)
            .ok_or("native driver has no compiled contract")?;
        let input = inputs
            .iter()
            .find(|input| {
                input.name == consumer.endpoint || input.port.as_deref() == Some(&consumer.endpoint)
            })
            .ok_or("native consumer has no compiled input")?;
        if input.delivery != InputDelivery::LeasedValue
            || input.request_fqn.as_deref() != Some("phoxal.component.actuator.v1.ActuatorCommand")
        {
            return Err(format!(
                "native implementation cannot serve authored driver input {consumer}"
            ));
        }
        let ComponentDocument::V0 { capabilities, .. } = &component.definition;
        let targets = capabilities
            .iter()
            .filter(|(_, capability)| capability.kind == "motor")
            .map(|(name, capability)| {
                if capability.target.kind != NativeTargetKind::Actuator {
                    return Err(format!(
                        "native motor {}.{name} is not an actuator",
                        instance.id
                    ));
                }
                Ok(format!("{}.{name}", instance.id))
            })
            .collect::<Result<BTreeSet<_>, String>>()?;
        if targets.is_empty() {
            return Err(format!(
                "native actuator input {consumer} has no declared motors"
            ));
        }
        for source in sources {
            routes
                .entry(source.clone())
                .or_default()
                .extend(targets.iter().cloned());
        }
    }
    Ok(routes)
}
