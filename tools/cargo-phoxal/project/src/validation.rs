//! Validation that requires the exact compiled Runtime contract.
//!
//! Cargo metadata identifies packages and targets, but it cannot expose the
//! associated `Runtime::Config` schema or port bindings.  This module builds
//! the selected executable targets, extracts their native contract records,
//! and validates authored values against the schema owned by that exact
//! artifact.  It never executes a target binary.

use std::collections::BTreeMap;
use std::fs;

use crate::artifact::{self, ArtifactContract};
use crate::cargo;
use crate::document::RobotDocument;
use crate::{CargoOptions, Error, PreparedProject};

pub(crate) type ArtifactKey = (String, String);

/// Build and inspect every selected runtime executable for a check command.
pub(crate) fn validate_selected_contracts(
    prepared: &PreparedProject,
    options: &CargoOptions,
) -> Result<BTreeMap<ArtifactKey, ArtifactContract>, Error> {
    options.validate()?;
    let mut contracts = BTreeMap::new();
    for (instance, target) in prepared.assembly_targets() {
        let key = (target.package_id.clone(), target.target.clone());
        if contracts.contains_key(&key) {
            continue;
        }
        let output = cargo::build_target(prepared, target, options)?;
        let executable = cargo::artifact_path(&output.stdout, target)?;
        let metadata = fs::symlink_metadata(&executable).map_err(|source| Error::ArtifactFile {
            path: executable.clone(),
            source,
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(Error::ArtifactInvalid {
                path: executable,
                message: "Cargo reported a non-regular executable".to_owned(),
            });
        }
        let contract = inspect_contract(prepared, &instance, target, &executable)?;
        contracts.insert(key, contract);
    }
    validate_configurations(prepared, &contracts)?;
    validate_connections(prepared, &contracts)?;
    Ok(contracts)
}

/// Validate authored service and component-driver configuration against the
/// exact contract selected for each instance.
pub(crate) fn validate_configurations(
    prepared: &PreparedProject,
    contracts: &BTreeMap<ArtifactKey, ArtifactContract>,
) -> Result<(), Error> {
    for (instance, target) in prepared.assembly_targets() {
        let key = (target.package_id.clone(), target.target.clone());
        let Some(contract) = contracts.get(&key) else {
            return Err(Error::ArtifactCapture {
                package: target.package.clone(),
                target: target.target.clone(),
                message: "selected executable has no extracted contract".to_owned(),
            });
        };
        let role = prepared.executable_role(&instance);
        let (field, mut value) = authored_configuration(prepared, &instance, &role)?;
        if schema_is_null(&contract.runtime.config_schema) && value.is_object() {
            // `Config = ()` has no authored fields.  Omission and an empty
            // object are the two source forms accepted for that declaration;
            // a non-empty object is still rejected by the exact schema below.
            if value.as_object().is_some_and(serde_json::Map::is_empty) {
                value = serde_json::Value::Null;
            }
        }
        let validator =
            jsonschema::validator_for(&contract.runtime.config_schema).map_err(|error| {
                Error::ConfigurationInvalid {
                    role: role.clone(),
                    instance: instance.clone(),
                    field: format!("{field} (compiled config_schema)"),
                    package: target.package.clone(),
                    message: error.to_string(),
                }
            })?;
        if let Some(error) = validator.iter_errors(&value).next() {
            return Err(Error::ConfigurationInvalid {
                role,
                instance,
                field,
                package: target.package.clone(),
                message: error.to_string(),
            });
        }
    }
    Ok(())
}

fn inspect_contract(
    prepared: &PreparedProject,
    instance: &str,
    target: &crate::SelectedTarget,
    executable: &std::path::Path,
) -> Result<ArtifactContract, Error> {
    artifact::inspect_file(executable).map_err(|error| {
        if matches!(error, artifact::Error::MissingRecord) {
            Error::MissingArtifactContract {
                role: prepared.executable_role(instance),
                instance: instance.to_owned(),
                package: target.package.clone(),
                target: target.target.clone(),
            }
        } else {
            Error::ArtifactInvalid {
                path: executable.to_owned(),
                message: error.to_string(),
            }
        }
    })
}

fn authored_configuration(
    prepared: &PreparedProject,
    instance: &str,
    role: &str,
) -> Result<(String, serde_json::Value), Error> {
    match role {
        "brain" => Ok(("brain.config".to_owned(), serde_json::Value::Null)),
        "service" => Ok((
            format!("services.{instance}.config"),
            prepared
                .document()
                .services
                .get(instance)
                .and_then(|selection| selection.config.clone())
                .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        )),
        "driver" => {
            let value = prepared
                .document()
                .robot
                .components
                .get(instance)
                .and_then(|component| component.driver.as_ref())
                .and_then(serde_json::Value::as_object)
                .and_then(|driver| driver.get("config"))
                .cloned()
                .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
            Ok((format!("robot.components.{instance}.driver.config"), value))
        }
        _ => Err(Error::ConfigurationInvalid {
            role: role.to_owned(),
            instance: instance.to_owned(),
            field: "configuration".to_owned(),
            package: "unknown".to_owned(),
            message: "selected target does not accept authored configuration".to_owned(),
        }),
    }
}

fn schema_is_null(schema: &serde_json::Value) -> bool {
    schema.get("type").and_then(serde_json::Value::as_str) == Some("null")
}

pub(crate) fn validate_connections(
    prepared: &PreparedProject,
    contracts: &BTreeMap<ArtifactKey, ArtifactContract>,
) -> Result<(), Error> {
    validate_connections_for_document(prepared, contracts, prepared.document())
}

pub(crate) fn validate_connections_for_document(
    prepared: &PreparedProject,
    contracts: &BTreeMap<ArtifactKey, ArtifactContract>,
    document: &RobotDocument,
) -> Result<(), Error> {
    validate_connections_for_document_with_virtual_producers(prepared, contracts, document, &[])
}

pub(crate) fn validate_connections_for_document_with_virtual_producers(
    prepared: &PreparedProject,
    contracts: &BTreeMap<ArtifactKey, ArtifactContract>,
    document: &RobotDocument,
    virtual_producers: &[&str],
) -> Result<(), Error> {
    let mut instance_contracts = BTreeMap::new();
    for (instance, target) in prepared.assembly_targets() {
        let key = (target.package_id.clone(), target.target.clone());
        if let Some(contract) = contracts.get(&key) {
            instance_contracts.insert(instance, contract.clone());
        }
    }
    artifact::validate_connected_endpoints_with_virtual_producers(
        document,
        &instance_contracts,
        virtual_producers,
    )
    .map_err(|error| Error::ArtifactInvalid {
        path: prepared.layout().robot_manifest().to_owned(),
        message: error.to_string(),
    })
}
