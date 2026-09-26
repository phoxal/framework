//! Declaration-level connection validation for manifest-authored services.
//!
//! `cargo phoxal check` uses this before Cargo runs: it resolves every
//! selected participant's `service.yaml` beside its prepared `api/` tree
//! through the same build-helper parser that generates code, then checks the
//! authored connections against the declared endpoints.  Edges involving a
//! participant without a service declaration (the robot brain or a legacy
//! Protobuf-service participant) are reported as deferred to the existing
//! compiled-contract validation at bundle construction.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use phoxal::artifact::document::{ConnectionSources, PortReference, RobotDocument, Source};
use phoxal_build::{DeclarationEvidence, Delivery, FILE_NAME, participant_declaration};

use super::{Error, PreparedProject};

/// Returns whether a package owns an endpoint declaration document: a
/// `service.yaml` or the sections embedded in its `component.yaml`.
fn unit_declaration(package: Option<&Path>) -> bool {
    let Some(package) = package else {
        return false;
    };
    package.join(FILE_NAME).is_file() || package.join("component.yaml").is_file()
}

/// Outcome of one declaration-level pass.
pub(crate) struct DeclarationReport {
    /// Connections validated against declarations.
    pub checked: usize,
    /// Connections deferred to compiled-contract validation, with reasons.
    pub deferred: Vec<String>,
}

pub(crate) fn validate(project: &PreparedProject) -> Result<DeclarationReport, Error> {
    let root = project.layout.root();
    let RobotDocument::V0 {
        services,
        robot,
        connections,
        ..
    } = &project.document;

    let mut api_roots: BTreeMap<&str, PathBuf> = BTreeMap::new();
    for (instance, selection) in services {
        api_roots.insert(instance, api_root(root, &selection.source));
    }
    for (instance, component) in &robot.components {
        if component.driver.is_some() {
            api_roots.insert(instance, api_root(root, &component.source));
        }
    }

    let mut declarations: BTreeMap<&str, DeclarationEvidence> = BTreeMap::new();
    for (instance, api_root) in &api_roots {
        if !api_root.is_dir() && !unit_declaration(api_root.parent()) {
            continue;
        }
        let evidence =
            participant_declaration(api_root).map_err(|error| Error::DeclarationCheck {
                message: format!("participant {instance}: {error}"),
            })?;
        if let Some(evidence) = evidence {
            declarations.insert(instance, evidence);
        }
    }
    insert_brain_declaration(project, &mut declarations)?;

    let mut report = DeclarationReport {
        checked: 0,
        deferred: Vec::new(),
    };
    for (field, sources) in connections {
        let projection = match sources {
            ConnectionSources::Projection(projection) => Some(projection),
            _ => None,
        };
        let sources = match sources {
            ConnectionSources::One(source) => std::slice::from_ref(source),
            ConnectionSources::Many(sources) => sources,
            ConnectionSources::Projection(projection) => std::slice::from_ref(&projection.from),
        };
        let consumer = PortReference::parse(field).map_err(|error| Error::DeclarationCheck {
            message: format!("{field}: {error}"),
        })?;
        let consumer_declaration = declarations.get(consumer.instance.as_str());
        let mut producer_declarations = Vec::new();
        for source in sources {
            let producer =
                PortReference::parse(source).map_err(|error| Error::DeclarationCheck {
                    message: format!("{source}: {error}"),
                })?;
            let evidence = declarations.get(producer.instance.as_str());
            producer_declarations.push((producer, evidence));
        }
        let manifest_sides = consumer_declaration.is_some() as usize
            + producer_declarations
                .iter()
                .filter(|(_, evidence)| evidence.is_some())
                .count();
        if manifest_sides == 0 {
            report.deferred.push(format!(
                "{field}: both participants declare endpoints only in compiled contracts"
            ));
            continue;
        }
        if manifest_sides != 1 + producer_declarations.len() {
            report.deferred.push(format!(
                "{field}: a participant declares no endpoint document; validated with compiled contracts at bundle time"
            ));
            continue;
        }
        let consumer_evidence = consumer_declaration.ok_or_else(|| Error::DeclarationCheck {
            message: format!("{field}: the consumer declares no endpoint document"),
        })?;
        match consumer_evidence.service.endpoint(&consumer.port) {
            None => {
                return Err(Error::DeclarationCheck {
                    message: format!(
                        "{field}: `{}` declares no endpoint named `{}`",
                        consumer.instance, consumer.port
                    ),
                });
            }
            Some((phoxal_build::EndpointSide::Input, _)) => {
                // A queued input merges ordered batches from every producer
                // (wheel encoder fan-in); a latest or leased input keeps one
                // replaceable source.
                let queued = consumer_evidence
                    .service
                    .input(&consumer.port)
                    .is_some_and(|input| input.delivery == Delivery::Queue);
                if producer_declarations.len() > 1 && !queued {
                    return Err(Error::DeclarationCheck {
                        message: format!(
                            "{field}: a data input accepts exactly one producer, found {}",
                            producer_declarations.len()
                        ),
                    });
                }
                for (producer, evidence) in &producer_declarations {
                    let evidence = evidence.ok_or_else(|| Error::DeclarationCheck {
                        message: format!(
                            "{field}: producer `{}` declares no endpoint document",
                            producer.instance
                        ),
                    })?;
                    let check = match projection {
                        Some(projection) => phoxal_build::check_projection_binding(
                            consumer_evidence,
                            &consumer.port,
                            evidence,
                            &producer.port,
                            &projection.map,
                        ),
                        None => phoxal_build::check_data_binding(
                            consumer_evidence,
                            &consumer.port,
                            evidence,
                            &producer.port,
                        ),
                    };
                    check.map_err(|error| Error::DeclarationCheck {
                        message: format!(
                            "{field} <- {}.{}: {error}",
                            producer.instance, producer.port
                        ),
                    })?;
                }
            }
            Some((phoxal_build::EndpointSide::Call, _)) => {
                if producer_declarations.len() != 1 {
                    return Err(Error::DeclarationCheck {
                        message: format!(
                            "{field}: a required operation accepts exactly one provider, found {}",
                            producer_declarations.len()
                        ),
                    });
                }
                let (producer, evidence) = &producer_declarations[0];
                let evidence = evidence.ok_or_else(|| Error::DeclarationCheck {
                    message: format!(
                        "{field}: producer `{}` declares no service.yaml",
                        producer.instance
                    ),
                })?;
                phoxal_build::check_call_binding(
                    consumer_evidence,
                    &consumer.port,
                    evidence,
                    &producer.port,
                )
                .map_err(|error| Error::DeclarationCheck {
                    message: format!(
                        "{field} <- {}.{}: {error}",
                        producer.instance, producer.port
                    ),
                })?;
            }
            Some((side, _)) => {
                return Err(Error::DeclarationCheck {
                    message: format!(
                        "{field}: endpoint `{}` is a {} and cannot consume connections",
                        consumer.port,
                        match side {
                            phoxal_build::EndpointSide::Output => "data output",
                            phoxal_build::EndpointSide::Served => "served operation",
                            phoxal_build::EndpointSide::Input
                            | phoxal_build::EndpointSide::Call => {
                                "consumer"
                            }
                        }
                    ),
                });
            }
        }
        report.checked += 1;
    }

    for (instance, evidence) in &declarations {
        for input in &evidence.service.inputs {
            let key = format!("{instance}.{}", input.name);
            if input.required && !connections.contains_key(&key) {
                return Err(Error::DeclarationCheck {
                    message: format!("required input `{key}` has no authored connection"),
                });
            }
        }
        for call in &evidence.service.calls {
            let key = format!("{instance}.{}", call.name);
            if call.required && !connections.contains_key(&key) {
                return Err(Error::DeclarationCheck {
                    message: format!("required call `{key}` has no authored connection"),
                });
            }
        }
    }
    Ok(report)
}

pub(crate) fn api_root(root: &Path, source: &Source) -> PathBuf {
    match source {
        Source::Path(path) => root.join(path).join("api"),
        Source::Package(package) => root
            .join(".phoxal/registry")
            .join(package.registry.as_deref().unwrap_or("phoxal"))
            .join(&package.name)
            .join(&package.version)
            .join("api"),
        Source::Git(git) => root
            .join(".phoxal/git")
            .join(&git.name)
            .join(&git.rev)
            .join("api"),
    }
}

/// Compares one executable's embedded contract with its service declaration.
///
/// Bundle construction calls this for every manifest-authored participant so
/// a stale or hand-edited declaration cannot compose against a binary that
/// was built from different endpoints.
pub(crate) fn check_executable_agreement(
    instance: &str,
    declaration: &DeclarationEvidence,
    contract: &super::artifact::ArtifactContract,
) -> Result<(), Error> {
    use phoxal::artifact::{InputRole, OutputRole, RuntimeRecord};

    let mismatch = |message: String| Error::DeclarationCheck {
        message: format!("executable for `{instance}` disagrees with service.yaml: {message}"),
    };
    let service = &declaration.service;
    let RuntimeRecord::V0 {
        inputs,
        transient_outputs,
        service_outputs,
        ..
    } = &contract.runtime;

    let mut expected_inputs: BTreeMap<&str, &phoxal::artifact::InputRecord> = inputs
        .iter()
        .map(|record| (record.name.as_str(), record))
        .collect();
    for input in &service.inputs {
        let record = expected_inputs.remove(input.name.as_str()).ok_or_else(|| {
            mismatch(format!(
                "input `{}` is missing from the executable",
                input.name
            ))
        })?;
        let expected_role = match (input.delivery, input.lease_valid_for_ms.is_some()) {
            (Delivery::Latest, false) => &InputRole::ObservationLatest,
            (Delivery::Latest, true) => &InputRole::LeasedValue,
            (Delivery::Queue, _) => &InputRole::ObservationHistory,
        };
        if &record.role != expected_role {
            return Err(mismatch(format!(
                "input `{}` has role {:?} but the declaration expects {expected_role:?}",
                input.name, record.role
            )));
        }
        if record.max_bytes != Some(input.max_bytes) {
            return Err(mismatch(format!(
                "input `{}` bounds {} differ from the declared {}",
                input.name,
                record.max_bytes.unwrap_or_default(),
                input.max_bytes
            )));
        }
        if input.delivery == Delivery::Queue
            && record.max_items != Some(input.max_items.unwrap_or(1))
        {
            return Err(mismatch(format!(
                "input `{}` item bounds differ from the declaration",
                input.name
            )));
        }
        if input.delivery == Delivery::Latest && record.max_age_ms != input.max_age_ms {
            return Err(mismatch(format!(
                "input `{}` age bounds {:?} differ from the declared {:?}",
                input.name, record.max_age_ms, input.max_age_ms
            )));
        }
        match input.lease_valid_for_ms {
            Some(valid_for_ms) => {
                let signature = record.signature.as_ref().ok_or_else(|| {
                    mismatch(format!(
                        "leased input `{}` carries no port signature",
                        input.name
                    ))
                })?;
                if record.port.as_deref() != Some(input.name.as_str())
                    || signature.lease_valid_for_ms != Some(valid_for_ms)
                    || signature.request != input.message.fqn
                {
                    return Err(mismatch(format!(
                        "leased input `{}` signature differs from the declaration",
                        input.name
                    )));
                }
            }
            None => {
                if record.response_fqn.as_deref() != Some(input.message.fqn.as_str()) {
                    return Err(mismatch(format!(
                        "input `{}` payload identity differs from the declaration",
                        input.name
                    )));
                }
            }
        }
    }
    for operation in service.operations.iter().chain(service.calls.iter()) {
        let record = expected_inputs
            .remove(operation.name.as_str())
            .ok_or_else(|| {
                mismatch(format!(
                    "endpoint `{}` is missing from the executable",
                    operation.name
                ))
            })?;
        let is_served = service
            .operations
            .iter()
            .any(|served| served.name == operation.name);
        let expected_role = if is_served {
            &InputRole::CallIngress
        } else {
            &InputRole::CallCompletions
        };
        if &record.role != expected_role {
            return Err(mismatch(format!(
                "endpoint `{}` has role {:?} but the declaration expects {expected_role:?}",
                operation.name, record.role
            )));
        }
        let signature = record.signature.as_ref().ok_or_else(|| {
            mismatch(format!(
                "endpoint `{}` carries no contract signature",
                operation.name
            ))
        })?;
        if signature.service != operation.contract
            || signature.request != operation.request.fqn
            || signature.response != operation.response.fqn
        {
            return Err(mismatch(format!(
                "endpoint `{}` contract identity differs from the declaration",
                operation.name
            )));
        }
        // Declared exchange bounds must survive into the executable: the
        // served operation's byte bound, and a required call's completion
        // outstanding and byte bounds.
        if record.max_bytes != Some(operation.max_bytes) {
            return Err(mismatch(format!(
                "endpoint `{}` byte bound {} differs from the declared {}",
                operation.name,
                record.max_bytes.unwrap_or_default(),
                operation.max_bytes
            )));
        }
        if !is_served && record.max_items != Some(operation.max_items) {
            return Err(mismatch(format!(
                "endpoint `{}` outstanding bound differs from the declaration",
                operation.name
            )));
        }
    }
    if let Some(record) = expected_inputs.values().next() {
        return Err(mismatch(format!(
            "executable input `{}` is absent from service.yaml",
            record.name
        )));
    }

    let mut expected_outputs: BTreeMap<&str, &phoxal::artifact::OutputRecord> = transient_outputs
        .iter()
        .chain(service_outputs.iter())
        .map(|record| (record.name.as_str(), record))
        .collect();
    for output in &service.outputs {
        let record = expected_outputs
            .remove(output.name.as_str())
            .ok_or_else(|| {
                mismatch(format!(
                    "output `{}` is missing from the executable",
                    output.name
                ))
            })?;
        if record.role != OutputRole::Method
            || record.port.as_deref() != Some(output.name.as_str())
            || record.signature.as_ref().is_none_or(|signature| {
                signature.service != output.message.fqn
                    || signature.response != output.message.fqn
                    || signature.retained_latest != output.retained_latest
                    || signature.lease_valid_for_ms != output.lease_valid_for_ms
            })
            || record.max_bytes != Some(output.max_bytes)
        {
            return Err(mismatch(format!(
                "output `{}` differs from the declaration",
                output.name
            )));
        }
    }
    for operation in &service.operations {
        let record = expected_outputs
            .remove(format!("{}_replies", operation.name).as_str())
            .ok_or_else(|| {
                mismatch(format!(
                    "replies for operation `{}` are missing from the executable",
                    operation.name
                ))
            })?;
        if record.role != OutputRole::Reply
            || record.input.as_deref() != Some(operation.name.as_str())
            || record.max_items != Some(operation.max_items)
            || record.max_bytes != Some(operation.max_bytes)
        {
            return Err(mismatch(format!(
                "replies for operation `{}` differ from the declaration",
                operation.name
            )));
        }
    }
    if let Some(record) = expected_outputs.values().next() {
        return Err(mismatch(format!(
            "executable output `{}` is absent from service.yaml",
            record.name
        )));
    }
    Ok(())
}

/// Resolves the brain's robot.yaml declaration and adds it to the evidence
/// map, so brain-side connection edges validate like any participant.
fn insert_brain_declaration(
    project: &PreparedProject,
    declarations: &mut BTreeMap<&str, DeclarationEvidence>,
) -> Result<(), Error> {
    let root = project.layout.root();
    let robot_source =
        std::fs::read(root.join("robot.yaml")).map_err(|error| Error::DeclarationCheck {
            message: format!("cannot read robot.yaml: {error}"),
        })?;
    let evidence = phoxal_build::brain_declaration(root, &robot_source).map_err(|error| {
        Error::DeclarationCheck {
            message: format!("brain: {error}"),
        }
    })?;
    if let Some(evidence) = evidence {
        declarations.insert("brain", evidence);
    }
    Ok(())
}

/// Resolves one instance's declaration, if it authored one.
pub(crate) fn declaration_for_instance(
    project: &PreparedProject,
    instance: &str,
) -> Result<Option<DeclarationEvidence>, Error> {
    let root = project.layout.root();
    let RobotDocument::V0 {
        services, robot, ..
    } = &project.document;
    let source = services
        .get(instance)
        .map(|selection| &selection.source)
        .or_else(|| {
            robot
                .components
                .get(instance)
                .filter(|component| component.driver.is_some())
                .map(|component| &component.source)
        });
    let Some(source) = source else {
        return Ok(None);
    };
    let api_root = api_root(root, source);
    if !api_root.is_dir() && !unit_declaration(api_root.parent()) {
        return Ok(None);
    }
    participant_declaration(&api_root).map_err(|error| Error::DeclarationCheck {
        message: format!("participant {instance}: {error}"),
    })
}

/// Compiles every explicit projection connection into the self-contained
/// bundle record the receiving runtime executes.
///
/// Each mapping is validated against both declarations and their descriptor
/// closures again here, so a bundle can never carry a projection its own
/// participants no longer declare.
pub(crate) fn compile_projections(
    project: &PreparedProject,
) -> Result<Vec<phoxal::artifact::bundle::ConnectionProjection>, Error> {
    let root = project.layout.root();
    let RobotDocument::V0 {
        services,
        robot,
        connections,
        ..
    } = &project.document;
    let mut declarations: BTreeMap<&str, DeclarationEvidence> = BTreeMap::new();
    let mut api_roots: BTreeMap<&str, PathBuf> = BTreeMap::new();
    for (instance, selection) in services {
        api_roots.insert(instance, api_root(root, &selection.source));
    }
    for (instance, component) in &robot.components {
        if component.driver.is_some() {
            api_roots.insert(instance, api_root(root, &component.source));
        }
    }
    for (instance, api_root) in &api_roots {
        if !api_root.is_dir() && !unit_declaration(api_root.parent()) {
            continue;
        }
        if let Some(evidence) =
            participant_declaration(api_root).map_err(|error| Error::DeclarationCheck {
                message: format!("participant {instance}: {error}"),
            })?
        {
            declarations.insert(instance, evidence);
        }
    }

    let mut compiled = Vec::new();
    for (field, sources) in connections {
        let ConnectionSources::Projection(projection) = sources else {
            continue;
        };
        let consumer = PortReference::parse(field).map_err(|error| Error::DeclarationCheck {
            message: format!("{field}: {error}"),
        })?;
        let producer =
            PortReference::parse(&projection.from).map_err(|error| Error::DeclarationCheck {
                message: format!("{}: {error}", projection.from),
            })?;
        let consumer_evidence = declarations
            .get(consumer.instance.as_str())
            .ok_or_else(|| Error::DeclarationCheck {
                message: format!("{field}: the consumer declares no endpoint document"),
            })?;
        let producer_evidence = declarations
            .get(producer.instance.as_str())
            .ok_or_else(|| Error::DeclarationCheck {
                message: format!("{}: the producer declares no service.yaml", projection.from),
            })?;
        phoxal_build::check_projection_binding(
            consumer_evidence,
            &consumer.port,
            producer_evidence,
            &producer.port,
            &projection.map,
        )
        .map_err(|error| Error::DeclarationCheck {
            message: format!("{field} <- {}: {error}", projection.from),
        })?;
        let input = consumer_evidence
            .service
            .input(&consumer.port)
            .ok_or_else(|| Error::DeclarationCheck {
                message: format!("{field}: the consumer declares no such input"),
            })?;
        let output = producer_evidence
            .service
            .output(&producer.port)
            .ok_or_else(|| Error::DeclarationCheck {
                message: format!("{}: the producer declares no such output", projection.from),
            })?;
        compiled.push(phoxal::artifact::bundle::ConnectionProjection {
            consumer_instance: consumer.instance.clone(),
            consumer_field: consumer.port.clone(),
            source_instance: producer.instance.clone(),
            source_port: producer.port.clone(),
            source_message: output.message.fqn.clone(),
            destination_message: input.message.fqn.clone(),
            map: projection.map.clone(),
            source_descriptors: producer_evidence.descriptors.clone(),
            destination_descriptors: consumer_evidence.descriptors.clone(),
        });
    }
    Ok(compiled)
}
