//! Deterministic compiled bundle assembly for one prepared robot project.
//!
//! The project compiler owns the source-side assembly boundary. It builds the
//! exact executable targets selected by robot.yaml, records the resolved Cargo
//! identities and authored inputs, and publishes one complete directory
//! atomically. This module deliberately does not launch a process, install a
//! release, or claim that a built executable is Ready.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use fs4::{FileExt, TryLockError};
use serde::Serialize;

use crate::project::artifact::{self, ArtifactSummary};
use crate::project::cargo;
use crate::project::selection::PackageSource;
use crate::project::validation;
use crate::project::{CargoOptions, Error, PreparedProject, RobotDocument};
use phoxal::artifact::RuntimeRecord;
use phoxal::scenario::__internal::{Action, Program};

// Re-exports from the framework artifact module. The module is the source of
// truth for every record the bundle exchanges; this module
// keeps the tool-only compiled bundle and assembly functions.
#[cfg(test)]
use phoxal::artifact::bundle::{BundleActuationBinding, SimulationProviderBinding};
pub use phoxal::artifact::bundle::{
    BundleComponent, BundleExecutable, BundleManifest, BundleModelAssets, BundlePackage,
    BundleSimulation, BundleSimulationProvider, SimulationModelFacts,
};

/// Test helper for constructing validated simulation model facts.
///
/// The framework artifact module owns the inert record, so the constructor must not
/// produce a tool-owned `Error`. Validation that depends on tool error
/// types stays here.
#[cfg(test)]
pub(crate) fn simulation_model_facts(
    model_identity: impl Into<String>,
    quantum_ns: u64,
    providers: Vec<SimulationProviderBinding>,
    actuation_bindings: Vec<BundleActuationBinding>,
) -> Result<SimulationModelFacts, Error> {
    let facts = SimulationModelFacts {
        model_identity: model_identity.into(),
        quantum_ns,
        providers,
        actuation_bindings,
    };
    validate_simulation_facts(&facts)?;
    Ok(facts)
}

const BIN_DIR: &str = "bin";
const ASSET_DIR: &str = "assets";
const MANIFEST_FILE: &str = "manifest.json";

/// A complete source-side compiled bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledBundle {
    root: PathBuf,
}

impl CompiledBundle {
    /// The published bundle directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve one selected executable by its bundle-relative instance name.
    #[must_use]
    pub fn executable(&self, instance: &str) -> PathBuf {
        self.root.join(BIN_DIR).join(instance)
    }
}

/// Run-only graph input used to validate a simulation specification without
/// changing the immutable robot bundle.
pub(crate) struct SimulationRunInput<'a> {
    pub(crate) program: &'a Program,
    pub(crate) fixture_instance_id: &'a str,
}

#[derive(Debug, Clone)]
struct StagedModel {
    closure: BundleModelAssets,
}

#[derive(Debug, Clone)]
struct StagedResource {
    name: String,
    bytes: Vec<u8>,
}

pub(crate) fn assemble_with_inputs(
    prepared: &PreparedProject,
    options: &CargoOptions,
    output: impl AsRef<Path>,
    include_native_assets: bool,
    simulation_facts: Option<&SimulationModelFacts>,
    simulation_run: Option<SimulationRunInput<'_>>,
) -> Result<CompiledBundle, Error> {
    options.validate()?;
    let output = output.as_ref();
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| Error::BundleDirectory {
        path: parent.to_owned(),
        source,
    })?;
    let _publication_lock = acquire_bundle_publication_lock(output)?;

    let staging = tempfile::Builder::new()
        .prefix(".phoxal-bundle-")
        .tempdir_in(parent)
        .map_err(|source| Error::BundleDirectory {
            path: parent.to_owned(),
            source,
        })?;
    let staged_root = staging.path();
    fs::create_dir(staged_root.join(BIN_DIR)).map_err(|source| Error::BundleDirectory {
        path: staged_root.join(BIN_DIR),
        source,
    })?;
    let (staged_model, component_sources) = if include_native_assets {
        (
            stage_model(prepared, staged_root)?,
            stage_component_models(prepared, staged_root)?,
        )
    } else {
        (None, BTreeMap::new())
    };

    let mut artifacts = BTreeMap::new();
    for (_, target) in prepared.assembly_targets() {
        let key = (target.package_id.clone(), target.target.clone());
        if artifacts.contains_key(&key) {
            continue;
        }
        let output = cargo::build_target(prepared, target, options)?;
        let executable = cargo::artifact_path(&output.stdout, target)?;
        ensure_regular_executable(&executable, &target.target)?;
        let instance = prepared
            .assembly_targets()
            .into_iter()
            .find(|(_, selected)| {
                selected.package_id == target.package_id && selected.target == target.target
            })
            .map(|(instance, _)| instance)
            .unwrap_or_else(|| target.target.clone());
        let role = prepared.executable_role(&instance);
        let contract = artifact::inspect_file(&executable).map_err(|error| {
            if matches!(error, artifact::Error::MissingRecord) {
                Error::MissingArtifactContract {
                    role: role.clone(),
                    instance: instance.clone(),
                    package: target.package.clone(),
                    target: target.target.clone(),
                }
            } else {
                Error::ArtifactInvalid {
                    path: executable.clone(),
                    message: error.to_string(),
                }
            }
        })?;
        artifacts.insert(key, (target.clone(), executable, contract));
    }

    let simulation_contracts = prepared
        .assembly_targets()
        .into_iter()
        .filter_map(|(instance, target)| {
            artifacts
                .get(&(target.package_id.clone(), target.target.clone()))
                .map(|(_, _, contract)| (instance, contract.summary()))
        })
        .collect::<BTreeMap<_, _>>();
    let mut executable_records = Vec::new();
    for (instance, target) in prepared.assembly_targets() {
        if simulation_facts.is_some() && prepared.executable_role(&instance) == "driver" {
            continue;
        }
        let key = (target.package_id.clone(), target.target.clone());
        let (built_target, source, contract) =
            artifacts.get(&key).ok_or_else(|| Error::ArtifactCapture {
                package: target.package.clone(),
                target: target.target.clone(),
                message: "Cargo did not produce a selected executable".to_owned(),
            })?;
        let destination_name = safe_bundle_name(&instance)?;
        let relative = format!("{BIN_DIR}/{destination_name}");
        let destination = staged_root.join(&relative);
        copy_executable(source, &destination)?;
        executable_records.push(BundleExecutable {
            role: prepared.executable_role(&instance),
            instance: instance.clone(),
            package_id: public_package_id(prepared, &built_target.package_id),
            package: built_target.package.clone(),
            target: built_target.target.clone(),
            path: relative,
            artifact: Some(contract.summary().into()),
        });
    }
    executable_records.sort_by(|left, right| left.path.cmp(&right.path));

    // The supervisor is part of the immutable bundle, but it is not a runtime
    // participant.  Keep it out of manifest.executables because the deployed
    // supervisor must never interpret its own binary as a child process.
    let supervisor_output =
        cargo::build_target(prepared, &prepared.cargo_sources().supervisor, options)?;
    let supervisor_source = cargo::artifact_path(
        &supervisor_output.stdout,
        &prepared.cargo_sources().supervisor,
    )?;
    ensure_regular_executable(&supervisor_source, "supervisor")?;

    let contract_map = artifacts
        .iter()
        .map(|(key, (_, _, contract))| (key.clone(), contract.clone()))
        .collect::<BTreeMap<_, _>>();
    let document = prepared.document().clone();
    let mut execution_document = document.clone();
    if let Some(scenario) = simulation_run.as_ref() {
        apply_scenario_substitutions(
            &mut execution_document,
            scenario.program,
            scenario.fixture_instance_id,
            &simulation_contracts,
        )?;
    }
    validation::validate_configurations(prepared, &contract_map)?;
    crate::project::artifact::validate_descriptor_closure_consistency(
        contract_map
            .iter()
            .map(|((_, instance), contract)| (instance.as_str(), contract)),
    )
    .map_err(|error| Error::ArtifactInvalid {
        path: prepared.cargo_manifest_path().to_owned(),
        message: error.to_string(),
    })?;
    let virtual_producers = simulation_run
        .as_ref()
        .map(|scenario| vec![scenario.fixture_instance_id])
        .unwrap_or_default();
    validation::validate_connections_for_document_with_virtual_producers(
        prepared,
        &contract_map,
        &execution_document,
        &virtual_producers,
    )?;

    let mut components = prepared
        .cargo_sources()
        .components
        .values()
        .map(|component| {
            Ok(BundleComponent {
                instance: component.instance.clone(),
                dependency_key: component.dependency_key.clone(),
                package_id: public_package_id(prepared, &component.package_id),
                package: component.package.clone(),
                source: source_identity(&component.source)?,
                mount_site: component.mount_site.clone(),
                definition: component.definition.clone(),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    components.sort_by(|left, right| left.instance.cmp(&right.instance));

    let supervisor_relative = format!("{BIN_DIR}/supervisor");
    copy_executable(&supervisor_source, &staged_root.join(&supervisor_relative))?;
    let target = effective_target(options);
    let profile = effective_profile(options);
    let features = effective_features(options);
    let simulation = simulation_facts
        .map(|facts| build_simulation_definition(prepared, facts, &simulation_contracts))
        .transpose()?;
    let RobotDocument::V0 { robot, .. } = prepared.document();
    let manifest = BundleManifest::V0 {
        robot_id: robot.id.clone(),
        root_package: BundlePackage {
            id: public_package_id(prepared, &prepared.cargo_root_package().id.to_string()),
            name: prepared.cargo_root_package().name.to_string(),
            source: "local".to_owned(),
        },
        target,
        profile,
        features,
        executables: executable_records,
        components,
        component_sources,
        model: staged_model.as_ref().map(|model| model.closure.clone()),
        simulation,
    };
    write_yaml(&staged_root.join("robot.yaml"), &document)?;
    write_json(&staged_root.join(MANIFEST_FILE), &manifest)?;

    let staged = staging.keep();
    publish_directory(&staged, output)?;
    Ok(CompiledBundle {
        root: output.to_owned(),
    })
}

fn apply_scenario_substitutions(
    document: &mut RobotDocument,
    program: &Program,
    fixture_instance_id: &str,
    contracts: &BTreeMap<String, ArtifactSummary>,
) -> Result<(), Error> {
    let RobotDocument::V0 { connections, .. } = document;
    if fixture_instance_id.is_empty() {
        return Err(simulation_error(
            "scenario fixture instance id must not be empty",
        ));
    }
    let mut substitutions = BTreeMap::<String, (String, phoxal::__private::PortSignature)>::new();
    for step in program.steps() {
        let (target_instance, signature) = match &step.action {
            Action::Setpoint {
                target_instance,
                consumer_signature,
                ..
            } => (target_instance, consumer_signature),
            Action::Withdraw {
                target_instance,
                producer_signature,
            } => (target_instance, producer_signature),
            Action::Command { .. } => continue,
        };
        let consumer = format!("{target_instance}.{}", signature.name);
        let replacement = format!("{fixture_instance_id}.{}", signature.name);
        if let Some(existing) =
            substitutions.insert(consumer.clone(), (replacement.clone(), *signature))
            && existing.0 != replacement
        {
            return Err(simulation_error(format!(
                "scenario program maps `{consumer}` to competing fixture producers"
            )));
        }
    }
    for (consumer, (replacement, signature)) in substitutions {
        let (target_instance, target_port) = consumer
            .split_once('.')
            .ok_or_else(|| simulation_error(format!("invalid scenario consumer `{consumer}`")))?;
        let contract = contracts.get(target_instance).ok_or_else(|| {
            simulation_error(format!(
                "scenario target `{consumer}` has no compiled input contract"
            ))
        })?;
        let RuntimeRecord::V0 { inputs, .. } = &contract.runtime;
        let input = inputs
            .iter()
            .find(|input| input.name == target_port || input.port.as_deref() == Some(target_port))
            .ok_or_else(|| {
                simulation_error(format!(
                    "scenario target `{consumer}` has no compiled input port `{target_port}`"
                ))
            })?;
        if input.role != crate::project::artifact::InputRole::LeasedValue {
            return Err(simulation_error(format!(
                "scenario substitution `{consumer}` is not a setpoint input"
            )));
        }
        let signature_matches = input
            .port
            .as_deref()
            .is_none_or(|port| port == signature.name)
            && input
                .request_fqn
                .as_deref()
                .is_none_or(|request| request == signature.request)
            && input.response_fqn.as_deref() == Some(signature.response)
            && signature.kind == phoxal::__private::PortKind::Setpoint;
        if !signature_matches {
            return Err(simulation_error(format!(
                "scenario substitution `{consumer}` expects {} -> {}, compiled consumer records {:?} -> {:?} on port {:?}",
                signature.request,
                signature.response,
                input.request_fqn,
                input.response_fqn,
                input.port
            )));
        }
        connections.insert(
            consumer,
            crate::project::document::ConnectionSources::One(replacement),
        );
    }
    Ok(())
}

fn build_simulation_definition(
    prepared: &PreparedProject,
    facts: &SimulationModelFacts,
    contracts: &BTreeMap<String, ArtifactSummary>,
) -> Result<BundleSimulation, Error> {
    const PROTOCOL: &str = "phoxal.simulation.v1";
    validate_simulation_facts(facts)?;

    let driver_instances = prepared
        .cargo_sources()
        .components
        .iter()
        .filter_map(|(instance, component)| component.driver.as_ref().map(|_| instance.clone()))
        .collect::<BTreeSet<_>>();
    if driver_instances.is_empty() {
        return Err(simulation_error(
            "simulation requires at least one selected physical driver to substitute",
        ));
    }

    let mut expected_provider_keys = BTreeSet::new();
    for instance in &driver_instances {
        let contract = contracts.get(instance).ok_or_else(|| {
            simulation_error(format!(
                "selected physical driver `{instance}` has no compiled contract"
            ))
        })?;
        for output in public_outputs(contract) {
            let Some(port) = output.port.as_ref() else {
                continue;
            };
            if is_observation_output(output) {
                expected_provider_keys.insert((instance.clone(), port.clone()));
            }
        }
    }
    let actual_provider_keys = facts
        .providers
        .iter()
        .map(|provider| (provider.service_instance.clone(), provider.port.clone()))
        .collect::<BTreeSet<_>>();
    if expected_provider_keys != actual_provider_keys {
        return Err(simulation_error(format!(
            "explicit simulation providers do not cover selected driver outputs (expected {:?}, got {:?})",
            expected_provider_keys, actual_provider_keys
        )));
    }
    for provider in &facts.providers {
        let contract = contracts.get(&provider.service_instance).ok_or_else(|| {
            simulation_error(format!(
                "simulation provider `{}` has no compiled driver contract",
                provider.service_instance
            ))
        })?;
        let output = public_output(contract, &provider.port).ok_or_else(|| {
            simulation_error(format!(
                "simulation provider `{}.{}` has no compiled output",
                provider.service_instance, provider.port
            ))
        })?;
        if !is_observation_output(output) {
            return Err(simulation_error(format!(
                "simulation provider `{}.{}` is not an observation output",
                provider.service_instance, provider.port
            )));
        }
        let signature = output.signature.as_ref().ok_or_else(|| {
            simulation_error(format!(
                "simulation provider `{}.{}` has no generated signature",
                provider.service_instance, provider.port
            ))
        })?;
        if provider.shape != crate::project::artifact::MethodShape::Observation
            || signature.shape != crate::project::artifact::MethodShape::Observation
            || provider.input_fqn != signature.request
            || provider.payload_fqn != signature.response
        {
            return Err(simulation_error(format!(
                "simulation provider `{}.{}` does not match its compiled generated signature",
                provider.service_instance, provider.port
            )));
        }
    }

    let expected_setpoint_keys = contracts
        .iter()
        .filter(|(instance, _)| !driver_instances.contains(*instance))
        .flat_map(|(instance, contract)| {
            public_outputs(contract)
                .filter(|output| is_leased_method_output(output))
                .filter_map(|output| output.port.clone().map(|port| (instance.clone(), port)))
        })
        .collect::<BTreeSet<_>>();
    let actual_binding_keys = facts
        .actuation_bindings
        .iter()
        .map(|binding| (binding.service_instance.clone(), binding.port.clone()))
        .collect::<BTreeSet<_>>();
    if !actual_binding_keys.is_subset(&expected_setpoint_keys) {
        return Err(simulation_error(format!(
            "simulation actuation bindings contain routes outside compiled setpoints (available {:?}, got {:?})",
            expected_setpoint_keys, actual_binding_keys
        )));
    }
    let prepared_document_connections = match prepared.document() {
        RobotDocument::V0 { connections, .. } => connections,
    };
    for (consumer, sources) in prepared_document_connections {
        let consumer = crate::project::document::PortReference::parse(consumer)
            .map_err(|error| simulation_error(error.to_string()))?;
        if !driver_instances.contains(&consumer.instance) {
            continue;
        }
        let RuntimeRecord::V0 { inputs, .. } = &contracts[&consumer.instance].runtime;
        let input = inputs
            .iter()
            .find(|input| input.name == consumer.port)
            .ok_or_else(|| simulation_error("native driver input has no compiled contract"))?;
        if input.role != crate::project::artifact::InputRole::LeasedValue {
            return Err(simulation_error(format!(
                "native substitution does not support driver input `{}.{}` with role {:?}",
                consumer.instance, consumer.port, input.role
            )));
        }
        for source in sources.as_slice() {
            let source = crate::project::document::PortReference::parse(source)
                .map_err(|error| simulation_error(error.to_string()))?;
            if !actual_binding_keys.contains(&(source.instance, source.port)) {
                return Err(simulation_error(format!(
                    "native substitution does not cover driver input `{}.{}`",
                    consumer.instance, consumer.port
                )));
            }
        }
    }
    for binding in &facts.actuation_bindings {
        let contract = contracts.get(&binding.service_instance).ok_or_else(|| {
            simulation_error(format!(
                "simulation actuation `{}.{}` has no compiled contract",
                binding.service_instance, binding.port
            ))
        })?;
        let output = public_output(contract, &binding.port).ok_or_else(|| {
            simulation_error(format!(
                "simulation actuation `{}.{}` has no compiled output",
                binding.service_instance, binding.port
            ))
        })?;
        if !is_leased_method_output(output) {
            return Err(simulation_error(format!(
                "simulation actuation `{}.{}` is not a generated setpoint",
                binding.service_instance, binding.port
            )));
        }
        let signature = output.signature.as_ref().ok_or_else(|| {
            simulation_error(format!(
                "simulation actuation `{}.{}` has no generated signature",
                binding.service_instance, binding.port
            ))
        })?;
        if binding.payload_fqn != signature.response {
            return Err(simulation_error(format!(
                "simulation actuation `{}.{}` does not match its compiled payload signature",
                binding.service_instance, binding.port
            )));
        }
    }

    let mut providers = facts
        .providers
        .iter()
        .map(|provider| {
            let contract = contracts.get(&provider.service_instance).ok_or_else(|| {
                simulation_error(format!(
                    "simulation provider `{}.{}` has no compiled contract",
                    provider.service_instance, provider.port
                ))
            })?;
            let output = public_output(contract, &provider.port).ok_or_else(|| {
                simulation_error(format!(
                    "simulation provider `{}.{}` has no compiled output",
                    provider.service_instance, provider.port
                ))
            })?;
            if provider.rate_microhertz == 0
                || u128::from(provider.rate_microhertz) * u128::from(facts.quantum_ns)
                    > 1_000_000_000_000_000
            {
                return Err(simulation_error(
                    "provider rate exceeds the native quantum or is zero",
                ));
            }
            let (max_message_bytes, max_buffered_items) = provider_bounds(output, &provider.port)?;
            let signature = output.signature.as_ref().ok_or_else(|| {
                simulation_error(format!(
                    "simulation provider {}.{} has no generated signature",
                    provider.service_instance, provider.port
                ))
            })?;
            Ok(BundleSimulationProvider {
                rate_microhertz: provider.rate_microhertz,
                service_instance: provider.service_instance.clone(),
                port: provider.port.clone(),
                service_fqn: signature.service.clone(),
                method: signature.method.clone(),
                shape: provider.shape,
                retained_latest: signature.retained_latest,
                lease_valid_for_ms: signature.lease_valid_for_ms,
                input_fqn: provider.input_fqn.clone(),
                payload_fqn: provider.payload_fqn.clone(),
                max_message_bytes,
                max_buffered_items,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    providers.sort_by(|left, right| {
        left.service_instance
            .cmp(&right.service_instance)
            .then_with(|| left.port.cmp(&right.port))
    });
    let mut actuation_bindings = facts.actuation_bindings.clone();
    actuation_bindings.sort_by(|left, right| {
        left.service_instance
            .cmp(&right.service_instance)
            .then_with(|| left.port.cmp(&right.port))
    });

    Ok(BundleSimulation {
        protocol: PROTOCOL.to_owned(),
        mode: "controlled".to_owned(),
        model_identity: facts.model_identity.clone(),
        quantum_ns: facts.quantum_ns,
        providers,
        actuation_bindings,
    })
}

fn public_outputs(
    contract: &ArtifactSummary,
) -> impl Iterator<Item = &crate::project::artifact::OutputRecord> {
    let RuntimeRecord::V0 {
        transient_outputs,
        service_outputs,
        ..
    } = &contract.runtime;
    transient_outputs.iter().chain(service_outputs.iter())
}

fn public_output<'a>(
    contract: &'a ArtifactSummary,
    port: &str,
) -> Option<&'a crate::project::artifact::OutputRecord> {
    public_outputs(contract).find(|output| output.port.as_deref() == Some(port))
}

fn is_observation_output(output: &crate::project::artifact::OutputRecord) -> bool {
    output.role == crate::project::artifact::OutputRole::Method
        && output.signature.as_ref().is_some_and(|signature| {
            signature.shape == crate::project::artifact::MethodShape::Observation
        })
}

fn is_leased_method_output(output: &crate::project::artifact::OutputRecord) -> bool {
    output.role == crate::project::artifact::OutputRole::Method
        && output
            .signature
            .as_ref()
            .is_some_and(|signature| signature.lease_valid_for_ms.is_some())
}

fn provider_bounds(
    output: &crate::project::artifact::OutputRecord,
    port: &str,
) -> Result<(u32, u32), Error> {
    let max_message_bytes = output.max_bytes.ok_or_else(|| {
        simulation_error(format!(
            "simulation provider `{port}` has no encoded-byte bound"
        ))
    })?;
    let max_message_bytes = u32::try_from(max_message_bytes).map_err(|_| {
        simulation_error(format!(
            "simulation provider `{port}` encoded-byte bound exceeds u32"
        ))
    })?;
    if max_message_bytes == 0 {
        return Err(simulation_error(format!(
            "simulation provider `{port}` encoded-byte bound must be positive"
        )));
    }
    let max_buffered_items = output
        .max_items
        .or_else(|| {
            output
                .signature
                .as_ref()
                .is_some_and(|signature| signature.retained_latest)
                .then_some(1)
        })
        .ok_or_else(|| {
            simulation_error(format!(
                "simulation provider `{port}` has no item-count bound"
            ))
        })?;
    let max_buffered_items = u32::try_from(max_buffered_items).map_err(|_| {
        simulation_error(format!(
            "simulation provider `{port}` item-count bound exceeds u32"
        ))
    })?;
    if max_buffered_items == 0 {
        return Err(simulation_error(format!(
            "simulation provider `{port}` item-count bound must be positive"
        )));
    }
    Ok((max_message_bytes, max_buffered_items))
}

fn validate_simulation_facts(facts: &SimulationModelFacts) -> Result<(), Error> {
    validate_simulation_identity(&facts.model_identity, "model identity")?;
    if facts.quantum_ns == 0 {
        return Err(simulation_error("simulation quantum must be positive"));
    }
    if facts.providers.is_empty() {
        return Err(simulation_error(
            "simulation provider set must not be empty",
        ));
    }
    let mut providers = BTreeSet::new();
    for provider in &facts.providers {
        validate_simulation_segment(&provider.service_instance, "provider service instance")?;
        validate_simulation_segment(&provider.port, "provider port")?;
        if provider.shape != crate::project::artifact::MethodShape::Observation {
            return Err(simulation_error(format!(
                "simulation provider `{}.{}` has an unsupported observation kind",
                provider.service_instance, provider.port
            )));
        }
        if !providers.insert((&provider.service_instance, &provider.port)) {
            return Err(simulation_error(format!(
                "simulation provider `{}.{}` is duplicated",
                provider.service_instance, provider.port
            )));
        }
        validate_simulation_fqn(
            &provider.input_fqn,
            &format!(
                "simulation provider `{}.{}` input message identity",
                provider.service_instance, provider.port
            ),
        )?;
        validate_simulation_fqn(
            &provider.payload_fqn,
            &format!(
                "simulation provider `{}.{}` payload message identity",
                provider.service_instance, provider.port
            ),
        )?;
    }
    if facts.actuation_bindings.is_empty() {
        return Err(simulation_error(
            "simulation actuation binding set must not be empty",
        ));
    }
    let mut bindings = BTreeSet::new();
    let mut actuators = BTreeSet::new();
    for binding in &facts.actuation_bindings {
        validate_simulation_segment(&binding.service_instance, "actuation service instance")?;
        validate_simulation_segment(&binding.port, "actuation port")?;
        if binding.payload_fqn.is_empty() || binding.actuator_ids.is_empty() {
            return Err(simulation_error(format!(
                "simulation actuation `{}.{}` must carry a payload identity and native actuator IDs",
                binding.service_instance, binding.port
            )));
        }
        if !bindings.insert((&binding.service_instance, &binding.port)) {
            return Err(simulation_error(format!(
                "simulation actuation `{}.{}` is duplicated",
                binding.service_instance, binding.port
            )));
        }
        for actuator in &binding.actuator_ids {
            if actuator.is_empty()
                || actuator.len() > 64
                || !actuator.is_ascii()
                || actuator.chars().any(char::is_whitespace)
            {
                return Err(simulation_error(format!(
                    "simulation actuation `{}.{}` contains an invalid native actuator ID",
                    binding.service_instance, binding.port
                )));
            }
            if !actuators.insert(actuator) {
                return Err(simulation_error(format!(
                    "native actuator `{actuator}` is mapped by more than one simulation output"
                )));
            }
        }
    }
    Ok(())
}

fn validate_simulation_segment(value: &str, field: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(simulation_error(format!(
            "{field} must be 1-64 lowercase ASCII letters, digits, '-' or '_'"
        )));
    }
    Ok(())
}

fn validate_simulation_identity(value: &str, field: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 512
        || !value.is_ascii()
        || value.chars().any(char::is_whitespace)
    {
        return Err(simulation_error(format!(
            "{field} must be a bounded non-whitespace ASCII identity"
        )));
    }
    Ok(())
}

fn validate_simulation_fqn(value: &str, field: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 512
        || !value.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .next()
                    .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
    {
        return Err(simulation_error(format!(
            "{field} must be a valid Protobuf message identity"
        )));
    }
    Ok(())
}

fn simulation_error(message: impl Into<String>) -> Error {
    Error::SimulationInvalid {
        message: message.into(),
    }
}

fn effective_target(options: &CargoOptions) -> String {
    cargo_arg_value(&options.cargo_args, "--target")
        .or_else(|| options.target.clone())
        .unwrap_or_else(|| "host".to_owned())
}

fn effective_profile(options: &CargoOptions) -> String {
    let mut profile = options.profile.clone();
    if options.release {
        profile = Some("release".to_owned());
    }
    let arguments = options
        .cargo_args
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--release" | "-r" => profile = Some("release".to_owned()),
            "--profile" => {
                if let Some(value) = arguments.get(index + 1) {
                    profile = Some(value.clone());
                    index += 1;
                }
            }
            value if value.starts_with("--profile=") => {
                profile = Some(value["--profile=".len()..].to_owned());
            }
            _ => {}
        }
        index += 1;
    }
    profile.unwrap_or_else(|| "dev".to_owned())
}

fn effective_features(options: &CargoOptions) -> Vec<String> {
    let mut features = options.features.clone();
    let arguments = options
        .cargo_args
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == "--features" {
            index += 1;
            while let Some(value) = arguments.get(index) {
                if value.starts_with('-') {
                    break;
                }
                features.extend(
                    value
                        .split(|character: char| character == ',' || character.is_whitespace())
                        .filter(|feature| !feature.is_empty())
                        .map(str::to_owned),
                );
                index += 1;
            }
            continue;
        }
        if let Some(value) = arguments[index].strip_prefix("--features=") {
            features.extend(
                value
                    .split(|character: char| character == ',' || character.is_whitespace())
                    .filter(|feature| !feature.is_empty())
                    .map(str::to_owned),
            );
        }
        index += 1;
    }
    features.sort();
    features.dedup();
    features
}

fn cargo_arg_value(arguments: &[std::ffi::OsString], name: &str) -> Option<String> {
    let arguments = arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let equals = format!("{name}=");
    let mut value = None;
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == name {
            value = arguments.get(index + 1).cloned();
            index += 1;
        } else if let Some(argument) = arguments[index].strip_prefix(&equals) {
            value = Some(argument.to_owned());
        }
        index += 1;
    }
    value
}

fn sanitize_git_source(source: &str, path: &Path) -> Result<String, Error> {
    let value = source
        .strip_prefix("git+")
        .ok_or_else(|| Error::ArtifactInvalid {
            path: path.to_owned(),
            message: "Git package source is missing Cargo's git source prefix".to_owned(),
        })?;
    let (repository, revision) = value
        .rsplit_once('#')
        .ok_or_else(|| Error::ArtifactInvalid {
            path: path.to_owned(),
            message: "Git package source is missing its resolved immutable revision".to_owned(),
        })?;
    let repository = repository
        .split_once('?')
        .map_or(repository, |(repository, _)| repository);
    let repository = safe_git_repository(repository, path)?;
    Ok(format!("git+{repository}#{revision}"))
}

fn safe_git_repository(repository: &str, path: &Path) -> Result<String, Error> {
    if let Some(authority) = repository.split_once("://").map(|(_, rest)| rest)
        && authority
            .split_once('/')
            .map_or(authority, |(authority, _)| authority)
            .contains('@')
    {
        return Err(Error::ArtifactInvalid {
            path: path.to_owned(),
            message: "Git source URL contains credentials".to_owned(),
        });
    }
    if repository.starts_with("file:") || repository.starts_with('/') {
        return Ok("local-git".to_owned());
    }
    Ok(repository.to_owned())
}

fn public_package_id(prepared: &PreparedProject, package_id: &str) -> String {
    if let Some(package) = prepared
        .cargo_metadata()
        .packages
        .iter()
        .find(|package| package.id.to_string() == package_id)
    {
        if package.source.is_none() {
            return format!("local:{}@{}", package.name, package.version);
        }
        if let Some(source) = package.source.as_ref()
            && source.repr.starts_with("git+")
        {
            let revision = source
                .repr
                .rsplit_once('#')
                .map_or("unknown", |(_, revision)| revision);
            return format!("git:{}@{}#{revision}", package.name, package.version);
        }
        return package.id.to_string();
    }
    if package_id.starts_with("path+") {
        "local".to_owned()
    } else if package_id.starts_with("git+file:") {
        "local-git".to_owned()
    } else {
        package_id.to_owned()
    }
}

fn stage_model(
    prepared: &PreparedProject,
    staged_root: &Path,
) -> Result<Option<StagedModel>, Error> {
    let RobotDocument::V0 { robot, .. } = prepared.document();
    let Some(path) = robot.model.as_ref() else {
        return Ok(None);
    };
    let relative = safe_input_path(path)?;
    let root = prepared.layout().root();
    let full = safe_source_file(root, &relative)?;
    let closure = closed_robot_model(root, &relative, &full)?;
    let mut resources = Vec::new();
    for resource in &closure.resources {
        let relative_path = format!("{ASSET_DIR}/{}", resource.name);
        let destination = staged_root.join(&relative_path);
        write_model_resource(&destination, resource)?;
        resources.push(relative_path);
    }
    Ok(Some(StagedModel {
        closure: BundleModelAssets {
            entry: format!("{ASSET_DIR}/{}", closure.entry),
            resources,
        },
    }))
}

fn stage_component_models(
    prepared: &PreparedProject,
    staged_root: &Path,
) -> Result<BTreeMap<String, String>, Error> {
    let mut paths = BTreeMap::new();
    for component in prepared.cargo_sources().components.values() {
        let source_root = component.source_root.clone();
        let phoxal::artifact::document::ComponentDocument::V0 { model, .. } = &component.definition;
        let entry = safe_input_path(&model.file)?;
        let full = safe_source_file(&source_root, &entry)?;
        let closure = closed_robot_model(&source_root, &entry, &full)?;
        let relative = format!(
            "{ASSET_DIR}/components/{}",
            safe_bundle_name(&component.instance)?
        );
        for resource in &closure.resources {
            write_model_resource(&staged_root.join(&relative).join(&resource.name), resource)?;
        }
        paths.insert(component.instance.clone(), relative);
    }
    Ok(paths)
}

pub(crate) fn retain_component_resources(source: &Path, destination: &Path) -> Result<(), Error> {
    let definition_path = source.join("component.yaml");
    let text = fs::read_to_string(&definition_path).map_err(|source| Error::ArtifactFile {
        path: definition_path.clone(),
        source,
    })?;
    let definition: phoxal::artifact::document::ComponentDocument = serde_yaml::from_str(&text)
        .map_err(|error| Error::ArtifactInvalid {
            path: definition_path.clone(),
            message: error.to_string(),
        })?;
    let phoxal::artifact::document::ComponentDocument::V0 { model, .. } = definition;
    let relative = safe_input_path(&model.file)?;
    let full = safe_source_file(source, &relative)?;
    let closure = closed_robot_model(source, &relative, &full)?;
    let model_root = relative
        .parent()
        .map_or(destination.to_owned(), |parent| destination.join(parent));
    for resource in &closure.resources {
        write_model_resource(&model_root.join(&resource.name), resource)?;
    }
    Ok(())
}

fn closed_robot_model(root: &Path, relative: &Path, full: &Path) -> Result<ModelClosure, Error> {
    let parent = relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        let resource_root = root.join(parent);
        let entry = relative
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                invalid_model(root, relative, "model path must have a UTF-8 file name")
            })?;
        let resources = collect_model_files(&resource_root, &resource_root, root, relative)?;
        return model_closure(entry, resources)
            .map_err(|message| invalid_model(root, relative, message));
    }

    let entry = relative
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid_model(root, relative, "model path must have a UTF-8 file name"))?;
    let mut resources = vec![StagedResource {
        name: entry.to_owned(),
        bytes: read_model_bytes(full, root, relative)?,
    }];
    let assets = root.join(ASSET_DIR);
    if assets.exists() {
        let metadata = fs::symlink_metadata(&assets).map_err(|source| Error::ArtifactFile {
            path: assets.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(invalid_model(
                root,
                relative,
                "model assets must be a regular directory",
            ));
        }
        resources.extend(collect_model_files(root, &assets, root, relative)?);
    }
    model_closure(entry, resources).map_err(|message| invalid_model(root, relative, message))
}

fn collect_model_files(
    resource_root: &Path,
    directory: &Path,
    root: &Path,
    model_relative: &Path,
) -> Result<Vec<StagedResource>, Error> {
    let mut entries = fs::read_dir(directory)
        .map_err(|source| Error::ArtifactFile {
            path: directory.to_owned(),
            source,
        })?
        .map(|entry| {
            entry
                .map_err(|source| Error::ArtifactFile {
                    path: directory.to_owned(),
                    source,
                })
                .and_then(|entry| {
                    let path = entry.path();
                    let metadata =
                        fs::symlink_metadata(&path).map_err(|source| Error::ArtifactFile {
                            path: path.clone(),
                            source,
                        })?;
                    let relative =
                        path.strip_prefix(resource_root)
                            .map_err(|_| Error::ArtifactInvalid {
                                path: path.clone(),
                                message: "model resource escaped its resource root".to_owned(),
                            })?;
                    Ok((relative.to_owned(), path, metadata))
                })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut resources = Vec::new();
    for (relative, path, metadata) in entries {
        if metadata.file_type().is_symlink() {
            return Err(invalid_model(
                root,
                model_relative,
                "model resources must not contain symlinks",
            ));
        }
        if metadata.is_dir() {
            resources.extend(collect_model_files(
                resource_root,
                &path,
                root,
                model_relative,
            )?);
        } else if metadata.is_file() {
            let name = normalize_model_resource_name(&relative)
                .map_err(|message| invalid_model(root, model_relative, message))?;
            let bytes = read_model_bytes(&path, root, model_relative)?;
            resources.push(StagedResource { name, bytes });
        } else {
            return Err(invalid_model(
                root,
                model_relative,
                "model resources must be regular files or directories",
            ));
        }
    }
    Ok(resources)
}

fn read_model_bytes(path: &Path, root: &Path, relative: &Path) -> Result<Vec<u8>, Error> {
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid_model(
            root,
            relative,
            "model resources must be regular files",
        ));
    }
    fs::read(path).map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })
}

fn write_model_resource(path: &Path, resource: &StagedResource) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::BundleDirectory {
            path: parent.to_owned(),
            source,
        })?;
    }
    let mut file = File::create(path).map_err(|source| Error::BundleWrite {
        path: path.to_owned(),
        source,
    })?;
    file.write_all(&resource.bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| Error::BundleWrite {
            path: path.to_owned(),
            source,
        })
}

fn model_closure(entry: &str, mut resources: Vec<StagedResource>) -> Result<ModelClosure, String> {
    if entry.is_empty() {
        return Err("model entry name must not be empty".to_owned());
    }
    normalize_model_resource_name(Path::new(entry))?;
    resources.sort_by(|left, right| left.name.cmp(&right.name));
    for pair in resources.windows(2) {
        if pair[0].name == pair[1].name {
            return Err(format!(
                "model resource {:?} appears more than once",
                pair[0].name
            ));
        }
    }
    if !resources.iter().any(|resource| resource.name == entry) {
        return Err(format!(
            "model entry {entry:?} is not present in the resource closure"
        ));
    }
    Ok(ModelClosure {
        entry: entry.to_owned(),
        resources,
    })
}

fn normalize_model_resource_name(path: &Path) -> Result<String, String> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.to_string_lossy().contains('\\')
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "model resource name {:?} must be relative, normalized, and use '/' separators",
            path
        ));
    }
    Ok(path.to_string_lossy().replace('\\', "/"))
}

#[derive(Debug, Clone)]
struct ModelClosure {
    entry: String,
    resources: Vec<StagedResource>,
}

fn invalid_model(root: &Path, relative: &Path, message: impl Into<String>) -> Error {
    Error::ArtifactInvalid {
        path: root.join(relative),
        message: message.into(),
    }
}

fn ensure_regular_executable(path: &Path, target: &str) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(Error::ArtifactInvalid {
            path: path.to_owned(),
            message: format!("Cargo reported a non-regular {target} executable"),
        });
    }
    Ok(())
}

fn copy_executable(source: &Path, destination: &Path) -> Result<(), Error> {
    fs::copy(source, destination).map_err(|source_error| Error::BundleCopy {
        from: source.to_owned(),
        to: destination.to_owned(),
        source: source_error,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(destination)
            .map_err(|source| Error::ArtifactFile {
                path: destination.to_owned(),
                source,
            })?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(destination, permissions).map_err(|source| Error::ArtifactFile {
            path: destination.to_owned(),
            source,
        })?;
    }
    Ok(())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|source| Error::BundleJson {
        path: path.to_owned(),
        source,
    })?;
    let mut file = File::create(path).map_err(|source| Error::BundleWrite {
        path: path.to_owned(),
        source,
    })?;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|source| Error::BundleWrite {
            path: path.to_owned(),
            source,
        })
}

fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let yaml = serde_yaml::to_string(value).map_err(|error| Error::ArtifactInvalid {
        path: path.to_owned(),
        message: format!("cannot encode compiled robot.yaml: {error}"),
    })?;
    let mut file = File::create(path).map_err(|source| Error::BundleWrite {
        path: path.to_owned(),
        source,
    })?;
    file.write_all(yaml.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|source| Error::BundleWrite {
            path: path.to_owned(),
            source,
        })
}

fn publish_directory(staged: &Path, output: &Path) -> Result<(), Error> {
    if output.exists() {
        if !output.is_dir() {
            return Err(Error::BundlePublish {
                path: output.to_owned(),
                source: io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "bundle output exists and is not a directory",
                ),
            });
        }
        if output.is_dir() && directories_equal(staged, output)? {
            fs::remove_dir_all(staged).map_err(|source| Error::BundleCleanup {
                path: staged.to_owned(),
                source,
            })?;
            return Ok(());
        }
        let old_parent = tempfile::Builder::new()
            .prefix(".phoxal-previous-")
            .tempdir_in(output.parent().unwrap_or_else(|| Path::new(".")))
            .map_err(|source| Error::BundleDirectory {
                path: output.to_owned(),
                source,
            })?;
        let old = old_parent.path().join("bundle");
        fs::rename(output, &old).map_err(|source| Error::BundlePublish {
            path: output.to_owned(),
            source,
        })?;
        if let Err(source) = fs::rename(staged, output) {
            let _ = fs::rename(&old, output);
            return Err(Error::BundlePublish {
                path: output.to_owned(),
                source,
            });
        }
        fs::remove_dir_all(&old).map_err(|source| Error::BundleCleanup { path: old, source })?;
        return Ok(());
    }
    fs::rename(staged, output).map_err(|source| Error::BundlePublish {
        path: output.to_owned(),
        source,
    })
}

struct BundlePublicationLock {
    file: File,
    key: PathBuf,
}

static ACTIVE_BUNDLE_PUBLICATION_LOCKS: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();

impl Drop for BundlePublicationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
        if let Some(active) = ACTIVE_BUNDLE_PUBLICATION_LOCKS.get()
            && let Ok(mut active) = active.lock()
        {
            active.remove(&self.key);
        }
    }
}

fn acquire_bundle_publication_lock(output: &Path) -> Result<BundlePublicationLock, Error> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let canonical_parent = parent
        .canonicalize()
        .map_err(|source| Error::BundleDirectory {
            path: parent.to_owned(),
            source,
        })?;
    let output_name = output.file_name().ok_or_else(|| Error::ArtifactInvalid {
        path: output.to_owned(),
        message: "bundle output must name a directory below an existing parent".to_owned(),
    })?;
    let lock_directory = canonical_parent.join(".phoxal-bundle-locks");
    fs::create_dir_all(&lock_directory).map_err(|source| Error::BundleDirectory {
        path: lock_directory.clone(),
        source,
    })?;
    let lock_path = lock_directory.join(format!("{}.lock", output_name.to_string_lossy()));
    let active = ACTIVE_BUNDLE_PUBLICATION_LOCKS.get_or_init(|| Mutex::new(BTreeSet::new()));
    let mut active = active.lock().map_err(|_| Error::BundleLock {
        path: lock_path.clone(),
        source: io::Error::other("bundle publication lock registry is poisoned"),
    })?;
    if active.contains(&lock_path) {
        return Err(Error::BundleBusy {
            path: output.to_owned(),
        });
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| Error::BundleLock {
            path: lock_path.clone(),
            source,
        })?;
    match FileExt::try_lock(&lock) {
        Ok(()) => {
            active.insert(lock_path.clone());
            Ok(BundlePublicationLock {
                file: lock,
                key: lock_path,
            })
        }
        Err(TryLockError::WouldBlock) => Err(Error::BundleBusy {
            path: output.to_owned(),
        }),
        Err(TryLockError::Error(source)) => Err(Error::BundleLock {
            path: lock_path,
            source,
        }),
    }
}

fn directories_equal(left: &Path, right: &Path) -> Result<bool, Error> {
    let left_entries = directory_entries(left)?;
    let right_entries = directory_entries(right)?;
    if left_entries != right_entries {
        return Ok(false);
    }
    for relative in left_entries {
        let left_path = left.join(&relative);
        let right_path = right.join(&relative);
        let left_metadata =
            fs::symlink_metadata(&left_path).map_err(|source| Error::ArtifactFile {
                path: left_path.clone(),
                source,
            })?;
        let right_metadata =
            fs::symlink_metadata(&right_path).map_err(|source| Error::ArtifactFile {
                path: right_path.clone(),
                source,
            })?;
        if left_metadata.is_dir() != right_metadata.is_dir() {
            return Ok(false);
        }
        if left_metadata.is_file()
            && fs::read(&left_path).map_err(|source| Error::ArtifactFile {
                path: left_path.clone(),
                source,
            })? != fs::read(&right_path).map_err(|source| Error::ArtifactFile {
                path: right_path.clone(),
                source,
            })?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn directory_entries(root: &Path) -> Result<BTreeSet<PathBuf>, Error> {
    let mut pending = vec![PathBuf::new()];
    let mut entries = BTreeSet::new();
    while let Some(relative) = pending.pop() {
        let directory = root.join(&relative);
        for entry in fs::read_dir(&directory).map_err(|source| Error::BundleDirectory {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| Error::BundleDirectory {
                path: directory.clone(),
                source,
            })?;
            let child = relative.join(entry.file_name());
            let metadata =
                fs::symlink_metadata(entry.path()).map_err(|source| Error::ArtifactFile {
                    path: entry.path(),
                    source,
                })?;
            if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
                return Err(Error::ArtifactInvalid {
                    path: entry.path(),
                    message: "bundle contains an unsupported filesystem entry".to_owned(),
                });
            }
            entries.insert(child.clone());
            if metadata.is_dir() {
                pending.push(child);
            }
        }
    }
    Ok(entries)
}

fn safe_bundle_name(value: &str) -> Result<String, Error> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_uppercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_')
        })
    {
        return Err(Error::ArtifactInvalid {
            path: PathBuf::from(value),
            message: "executable instance is not a safe bundle filename".to_owned(),
        });
    }
    Ok(value.to_owned())
}

fn safe_input_path(path: &Path) -> Result<PathBuf, Error> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(Error::ArtifactInvalid {
            path: path.to_owned(),
            message: "authored input path must remain below the robot root".to_owned(),
        });
    }
    Ok(path.to_owned())
}

fn safe_source_file(root: &Path, relative: &Path) -> Result<PathBuf, Error> {
    let root = root.canonicalize().map_err(|source| Error::ArtifactFile {
        path: root.to_owned(),
        source,
    })?;
    let full = root.join(relative);
    let mut current = root.clone();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(Error::ArtifactInvalid {
                path: full,
                message: "authored input path must be normalized".to_owned(),
            });
        };
        current.push(part);
        let metadata = fs::symlink_metadata(&current).map_err(|source| Error::ArtifactFile {
            path: current.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(Error::ArtifactInvalid {
                path: current,
                message: "authored model path must not contain symlinks".to_owned(),
            });
        }
    }
    let canonical = full.canonicalize().map_err(|source| Error::ArtifactFile {
        path: full.clone(),
        source,
    })?;
    if !canonical.starts_with(&root) {
        return Err(Error::ArtifactInvalid {
            path: full,
            message: "authored input resolves outside the robot root".to_owned(),
        });
    }
    Ok(canonical)
}

fn source_identity(source: &PackageSource) -> Result<String, Error> {
    match source {
        PackageSource::Local { .. } => Ok("local".to_owned()),
        PackageSource::Git { source } => sanitize_git_source(source, Path::new("Cargo.toml")),
        PackageSource::Registry { source } | PackageSource::Other { source } => Ok(source.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simulation_facts() -> SimulationModelFacts {
        simulation_model_facts(
            "model-digest",
            10_000_000,
            vec![SimulationProviderBinding {
                rate_microhertz: 100_000_000,
                service_instance: "imu".to_owned(),
                port: "sample".to_owned(),
                shape: crate::project::artifact::MethodShape::Observation,
                retained_latest: false,
                lease_valid_for_ms: None,
                input_fqn: "google.protobuf.Empty".to_owned(),
                payload_fqn: "example.Imu".to_owned(),
            }],
            vec![BundleActuationBinding {
                service_instance: "motion".to_owned(),
                port: "actuators".to_owned(),
                payload_fqn: "example.Actuators".to_owned(),
                actuator_ids: vec!["motor".to_owned()],
            }],
        )
        .expect("valid simulation facts")
    }

    #[test]
    fn simulation_facts_require_explicit_provider_and_actuator_sets() {
        let error = simulation_model_facts("model-digest", 10_000_000, Vec::new(), Vec::new())
            .expect_err("empty native facts must fail before bundle assembly");
        assert!(
            matches!(error, Error::SimulationInvalid { message } if message.contains("provider set"))
        );
    }

    #[test]
    fn simulation_facts_reject_duplicate_native_actuators() {
        let mut facts = simulation_facts();
        facts.actuation_bindings.push(BundleActuationBinding {
            service_instance: "motion-aux".to_owned(),
            port: "actuators".to_owned(),
            payload_fqn: "example.Actuators".to_owned(),
            actuator_ids: vec!["motor".to_owned()],
        });
        let error = validate_simulation_facts(&facts)
            .expect_err("one native actuator cannot have two generated owners");
        assert!(
            matches!(error, Error::SimulationInvalid { message } if message.contains("mapped by more than one"))
        );
    }

    #[test]
    fn simulation_facts_round_trip_without_losing_explicit_bindings() {
        let facts = simulation_facts();
        let encoded = serde_json::to_value(&facts).expect("simulation facts JSON");
        let decoded: SimulationModelFacts =
            serde_json::from_value(encoded).expect("simulation facts decode");
        assert_eq!(decoded, facts);
    }

    #[test]
    fn bundle_names_reject_path_traversal_and_empty_values() {
        assert!(safe_bundle_name("../outside").is_err());
        assert!(safe_bundle_name("").is_err());
        assert_eq!(
            safe_bundle_name("front_service").expect("valid name"),
            "front_service"
        );
    }

    #[test]
    fn source_identity_does_not_leak_local_absolute_paths() {
        assert_eq!(
            source_identity(&PackageSource::Local {
                manifest_path: PathBuf::from("/private/checkout/Cargo.toml"),
            })
            .expect("local identity"),
            "local"
        );
    }

    #[test]
    fn git_source_identity_redacts_local_paths_and_rejects_credentials() {
        let path = Path::new("Cargo.toml");
        assert_eq!(
            safe_git_repository("file:///private/checkout", path).expect("local URL"),
            "local-git"
        );
        assert!(matches!(
            safe_git_repository("https://user:secret@example.invalid/repo", path),
            Err(Error::ArtifactInvalid { message, .. }) if message.contains("credentials")
        ));
    }

    #[test]
    fn authored_input_paths_cannot_escape_the_robot_root() {
        assert!(safe_input_path(Path::new("../model.xml")).is_err());
        assert!(safe_input_path(Path::new("/tmp/model.xml")).is_err());
        assert_eq!(
            safe_input_path(Path::new("models/robot.xml")).expect("relative path"),
            PathBuf::from("models/robot.xml")
        );
    }

    #[test]
    fn bundle_publication_is_serialized_per_output() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let output = directory.path().join("bundle");
        let held = acquire_bundle_publication_lock(&output).expect("first publication lock");
        assert!(matches!(
            acquire_bundle_publication_lock(&output),
            Err(Error::BundleBusy { path }) if path == output
        ));
        drop(held);
        acquire_bundle_publication_lock(&output).expect("released publication lock");
    }
}
