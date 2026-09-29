//! Lower mismatched latest observations into one robot-owned runtime target.
//!
//! This stage runs after exact participant preparation and before Cargo reads
//! the root package targets. The author's `robot.yaml` remains the source of
//! connection intent; the returned document is the executable graph, and the
//! conversion edges are persisted as a sidecar plan so the brain's
//! `build.rs` can emit the conversion runtime into `OUT_DIR` at compile
//! time. There is no generated `src/bin/phoxal-adapter.rs` and no generated
//! Cargo target or manifest.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use phoxal::artifact::document::{
    ConnectionSources, PortReference, RobotDocument, ServiceSelection, Source,
};
use phoxal::artifact::{InputRole, MethodShape, RuntimeRecord};
use phoxal::build::ConversionEdge;

use super::{CargoOperation, CargoOptions, Error, ProjectLayout, cargo, manifest_check, selection};

pub(super) const INSTANCE: &str = "phoxal-adapter";

#[derive(Clone)]
struct Edge {
    consumer: String,
    producer: String,
    source_type: String,
    destination_type: String,
    max_age_ms: Option<u64>,
    input_max_bytes: u64,
    output_max_bytes: u64,
    period_ms: u64,
    timeout_ms: u64,
    init_timeout_ms: u64,
}

impl From<Edge> for ConversionEdge {
    fn from(edge: Edge) -> Self {
        Self {
            consumer: edge.consumer,
            producer: edge.producer,
            source_type: edge.source_type,
            destination_type: edge.destination_type,
            max_age_ms: edge.max_age_ms,
            input_max_bytes: edge.input_max_bytes,
            output_max_bytes: edge.output_max_bytes,
            period_ms: edge.period_ms,
            timeout_ms: edge.timeout_ms,
            init_timeout_ms: edge.init_timeout_ms,
        }
    }
}

/// Generate the adapter target, when an exact selected output does not match
/// its latest-value consumer, and return the graph that executes it.
pub(crate) fn lower(
    layout: &ProjectLayout,
    authored: &RobotDocument,
    options: &CargoOptions,
) -> Result<RobotDocument, Error> {
    let RobotDocument::V0 {
        robot,
        services,
        connections,
        ..
    } = authored;
    let mut contracts = BTreeMap::new();
    let mut brain_binary: Option<String> = None;
    for (instance, selection) in services {
        if let Some(contract) = read_contract(
            layout.root(),
            &selection.source,
            selection.binary.as_deref(),
        )? {
            contracts.insert(instance.as_str(), contract);
        }
    }
    for (instance, component) in &robot.components {
        if component.driver.is_some()
            && let Some(contract) = read_contract(
                layout.root(),
                &component.source,
                component.binary.as_deref(),
            )?
        {
            contracts.insert(instance.as_str(), contract);
        }
    }
    if connections.iter().any(|(consumer, sources)| {
        PortReference::parse(consumer).is_ok_and(|port| port.instance == "brain")
            || matches!(sources, ConnectionSources::One(source)
                if PortReference::parse(source).is_ok_and(|port| port.instance == "brain"))
    }) {
        let (record, binary) = read_brain_contract(layout, authored, options)?;
        contracts.insert("brain", record);
        brain_binary = Some(binary);
    }

    let mut edges = Vec::new();
    for (consumer_text, sources) in connections {
        let ConnectionSources::One(producer_text) = sources else {
            continue;
        };
        let Ok(consumer) = PortReference::parse(consumer_text) else {
            continue;
        };
        let Ok(producer) = PortReference::parse(producer_text) else {
            continue;
        };
        let (Some(consumer_contract), Some(producer_contract)) = (
            contracts.get(consumer.instance.as_str()),
            contracts.get(producer.instance.as_str()),
        ) else {
            continue;
        };
        let RuntimeRecord::V0 {
            inputs,
            period_ms,
            timeout_ms,
            init_timeout_ms,
            ..
        } = consumer_contract;
        let Some(input) = inputs.iter().find(|input| input.name == consumer.port) else {
            continue;
        };
        let RuntimeRecord::V0 {
            service_outputs,
            transient_outputs,
            ..
        } = producer_contract;
        let Some(output) = service_outputs
            .iter()
            .chain(transient_outputs)
            .find(|output| {
                output
                    .signature
                    .as_ref()
                    .is_some_and(|signature| signature.endpoint == producer.port)
            })
        else {
            continue;
        };
        let Some(signature) = output.signature.as_ref() else {
            continue;
        };
        if input.role != InputRole::ObservationLatest || signature.shape != MethodShape::Observation
        {
            // Calls and leased commands compare their request payloads in
            // ordinary compiled connection validation, not this adapter.
            continue;
        }
        let Some(destination_type) = input.response_fqn.as_deref() else {
            continue;
        };
        if destination_type == signature.response {
            continue;
        }
        if !signature.retained_latest
            || signature.request != "google.protobuf.Empty"
            || signature.lease_valid_for_ms.is_some()
        {
            return Err(Error::DeclarationCheck {
                message: format!(
                    "{consumer_text} <- {producer_text}: different payloads require a latest-observation conversion; this delivery shape has no robot adapter yet"
                ),
            });
        }
        edges.push(Edge {
            consumer: consumer_text.clone(),
            producer: producer_text.clone(),
            source_type: signature.response.clone(),
            destination_type: destination_type.to_owned(),
            max_age_ms: input.max_age_ms,
            input_max_bytes: output.max_bytes.ok_or_else(|| Error::DeclarationCheck {
                message: format!(
                    "{producer_text}: a converted observation needs a compiled output byte bound"
                ),
            })?,
            output_max_bytes: input.max_bytes.ok_or_else(|| Error::DeclarationCheck {
                message: format!(
                    "{consumer_text}: a converted observation needs a compiled input byte bound"
                ),
            })?,
            period_ms: *period_ms,
            timeout_ms: *timeout_ms,
            init_timeout_ms: *init_timeout_ms,
        });
    }

    if edges.is_empty() {
        let plan = phoxal::build::ConversionPlan { edges: Vec::new() };
        plan.write(layout.root())
            .map_err(|error| Error::DeclarationCheck {
                message: format!(
                    "cannot persist empty conversion plan at {}: {error}",
                    phoxal::build::ConversionPlan::sidecar(layout.root()).display()
                ),
            })?;
        return Ok(authored.clone());
    }
    if services.contains_key(INSTANCE) || robot.components.contains_key(INSTANCE) {
        return Err(Error::DeclarationCheck {
            message: format!("{INSTANCE} is reserved for the generated robot conversion target"),
        });
    }
    let plan = phoxal::build::ConversionPlan {
        edges: edges.iter().cloned().map(ConversionEdge::from).collect(),
    };
    plan.write(layout.root())
        .map_err(|error| Error::DeclarationCheck {
            message: format!(
                "cannot persist conversion plan at {}: {error}",
                phoxal::build::ConversionPlan::sidecar(layout.root()).display()
            ),
        })?;
    // The conversion role executes inside the robot's own brain
    // executable. The executable graph keeps exactly the shape a separate
    // adapter target produced — a dedicated conversion instance between
    // producer and consumer — but the `phoxal-adapter` service selects the
    // brain's Cargo target instead of a generated one: the brain's build
    // helper hosts the conversion runtime from the persisted plan, bundle
    // assembly reads its named hosted record out of the brain binary, and
    // the supervisor launches this same executable once more under the
    // `phoxal-adapter` instance id.
    let brain_binary = match brain_binary {
        Some(binary) => binary,
        None => resolve_brain_binary(layout, authored, options)?,
    };
    let mut executable = authored.clone();
    let RobotDocument::V0 {
        services,
        connections,
        ..
    } = &mut executable;
    services.insert(
        INSTANCE.to_owned(),
        ServiceSelection {
            source: Source::Path(".".to_owned()),
            binary: Some(brain_binary),
            config: None,
        },
    );
    for (index, edge) in edges.iter().enumerate() {
        connections.insert(
            format!("{INSTANCE}.source_{index}"),
            ConnectionSources::One(edge.producer.clone()),
        );
        connections.insert(
            edge.consumer.clone(),
            ConnectionSources::One(format!("{INSTANCE}.target_{index}")),
        );
    }
    Ok(executable)
}

/// Resolves the brain's Cargo binary name without building it.
fn resolve_brain_binary(
    layout: &ProjectLayout,
    authored: &RobotDocument,
    options: &CargoOptions,
) -> Result<String, Error> {
    let metadata = cargo::load_metadata_at(layout.cargo_manifest(), layout.root(), None, options)?;
    let root = metadata
        .root_package()
        .ok_or(super::error::SourceError::MissingBrain)?;
    let RobotDocument::V0 { brain, .. } = authored;
    let target = selection::resolve_brain(root, brain.as_ref(), &metadata)?;
    Ok(target.target)
}

fn read_brain_contract(
    layout: &ProjectLayout,
    authored: &RobotDocument,
    options: &CargoOptions,
) -> Result<(RuntimeRecord, String), Error> {
    let metadata = cargo::load_metadata_at(layout.cargo_manifest(), layout.root(), None, options)?;
    let root = metadata
        .root_package()
        .ok_or(super::error::SourceError::MissingBrain)?;
    let RobotDocument::V0 { brain, .. } = authored;
    let target = selection::resolve_brain(root, brain.as_ref(), &metadata)?;
    let binary = target.target.clone();
    let mut command = Command::new(options.cargo_program());
    command
        .current_dir(layout.root())
        .args(["build", "--manifest-path"]);
    command.arg(layout.cargo_manifest());
    if let Some(config) = cargo::registry_config(layout.root()) {
        command.args(["--config", &config]);
    }
    let mut build_options = options.clone();
    build_options.cargo_args.clear();
    build_options.test_args.clear();
    build_options.append_common(&mut command, false, false);
    command.args(["--package", &target.package, "--bin", &target.target]);
    command.args(["--message-format", "json-render-diagnostics"]);
    let output = cargo::run_command(command, CargoOperation::Build)?;
    let executable = cargo::artifact_path(&output.stdout, &target)?;
    let contract = super::artifact::inspect_file(&executable).map_err(|error| match error {
        super::artifact::Error::MissingRecord => Error::MissingArtifactContract {
            role: "brain".to_owned(),
            instance: "brain".to_owned(),
            package: target.package.clone(),
            target: target.target.clone(),
        },
        other => Error::ArtifactInvalid {
            path: executable.clone(),
            message: other.to_string(),
        },
    })?;
    let descriptors = super::participant::merge_descriptor_closures(&contract)?;
    let prepared_dir = phoxal_build::self_prepared_dir(layout.root());
    let selection = phoxal_build::PreparedSelection::SelfHosted {
        package: target.package.clone(),
    };
    let executable_record = phoxal_build::PreparedExecutable {
        sha256: super::participant::digest_of(&executable)?,
        package: target.package.clone(),
        version: Some(root.version.to_string()),
    };
    if !phoxal_build::read_prepared(&prepared_dir)
        .is_ok_and(|prepared| prepared.file.executable == executable_record)
    {
        super::participant::write_prepared_contract(
            &prepared_dir,
            &selection,
            None,
            &contract.runtime,
            &descriptors,
            &executable_record,
        )?;
    }
    Ok((contract.runtime, binary))
}

fn read_contract(
    root: &Path,
    source: &Source,
    binary: Option<&str>,
) -> Result<Option<RuntimeRecord>, Error> {
    let contract_dir = manifest_check::prepared_root(root, source, binary);
    if !contract_dir.join(phoxal_build::CONTRACT_FILE).is_file() {
        return Ok(None);
    }
    let prepared = phoxal_build::read_prepared_for(
        &contract_dir,
        &manifest_check::selection_identity(source),
        binary,
    )
    .map_err(|error| Error::DeclarationCheck {
        message: format!("{error}"),
    })?;
    let record =
        serde_json::from_value(prepared.file.runtime).map_err(|error| Error::DeclarationCheck {
            message: format!(
                "invalid prepared runtime {}: {error}",
                contract_dir.display()
            ),
        })?;
    Ok(Some(record))
}
