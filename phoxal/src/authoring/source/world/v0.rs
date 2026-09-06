//! Exact `phoxal/world/v0` authored grammar.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::identity::is_valid_token;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub assets: BTreeMap<String, Asset>,
    pub world: World,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub geometry: Geometry,
    pub collision: Option<Geometry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Geometry {
    Box {
        size: [f64; 3],
    },
    Cylinder {
        radius: f64,
        length: f64,
    },
    Capsule {
        radius: f64,
        length: f64,
    },
    Sphere {
        radius: f64,
    },
    Mesh {
        path: PathBuf,
        scale: Option<[f64; 3]>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct World {
    pub id: String,
    pub time_step_ms: u64,
    pub gravity_mps2: [f64; 3],
    #[serde(default)]
    pub spawn_points: BTreeMap<String, Pose>,
    #[serde(default)]
    pub entities: BTreeMap<String, EntityDeclaration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pose {
    pub xyz: [f64; 3],
    pub rpy: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityDeclaration {
    pub asset: String,
    #[schemars(length(min = 1))]
    pub instances: Vec<EntityInstance>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityInstance {
    pub pose: Pose,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("{kind} '{name}' must use the normalized topology-token grammar")]
    InvalidName { kind: &'static str, name: String },
    #[error("world.time_step_ms must be positive and exactly convertible to nanoseconds")]
    InvalidTimeStep,
    #[error("{path} must contain only finite values")]
    NonFinite { path: String },
    #[error("{path} dimensions and absolute scale components must be positive")]
    InvalidDimensions { path: String },
    #[error("{path} mesh path must be relative and name a .glb file")]
    InvalidMeshPath { path: String },
    #[error("world.entities.{declaration}.asset references unknown asset '{asset}'")]
    UnknownAsset { declaration: String, asset: String },
    #[error("world.entities.{declaration}.instances must contain at least one pose")]
    EmptyInstances { declaration: String },
    #[error("world.entities.{declaration}.instances exceeds the supported u32 index range")]
    TooManyInstances { declaration: String },
}

impl Manifest {
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        validate_name("world id", &self.world.id, &mut errors);
        for name in self.assets.keys() {
            validate_name("world asset name", name, &mut errors);
        }
        for name in self.world.spawn_points.keys() {
            validate_name("world spawn name", name, &mut errors);
        }
        for name in self.world.entities.keys() {
            validate_name("world entity declaration name", name, &mut errors);
        }
        if self.world.time_step_ms == 0 || self.world.time_step_ms.checked_mul(1_000_000).is_none()
        {
            errors.push(ValidationError::InvalidTimeStep);
        }
        finite("world.gravity_mps2", self.world.gravity_mps2, &mut errors);
        for (name, pose) in &self.world.spawn_points {
            validate_pose(&format!("world.spawn_points.{name}"), *pose, &mut errors);
        }
        for (name, asset) in &self.assets {
            validate_geometry(
                &format!("assets.{name}.geometry"),
                &asset.geometry,
                &mut errors,
            );
            if let Some(collision) = &asset.collision {
                validate_geometry(&format!("assets.{name}.collision"), collision, &mut errors);
            }
        }
        for (name, declaration) in &self.world.entities {
            if !self.assets.contains_key(&declaration.asset) {
                errors.push(ValidationError::UnknownAsset {
                    declaration: name.clone(),
                    asset: declaration.asset.clone(),
                });
            }
            if declaration.instances.is_empty() {
                errors.push(ValidationError::EmptyInstances {
                    declaration: name.clone(),
                });
            }
            if u32::try_from(declaration.instances.len()).is_err() {
                errors.push(ValidationError::TooManyInstances {
                    declaration: name.clone(),
                });
            }
            for (index, instance) in declaration.instances.iter().enumerate() {
                validate_pose(
                    &format!("world.entities.{name}.instances[{index}].pose"),
                    instance.pose,
                    &mut errors,
                );
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

fn validate_name(kind: &'static str, name: &str, errors: &mut Vec<ValidationError>) {
    if !is_valid_token(name) {
        errors.push(ValidationError::InvalidName {
            kind,
            name: name.to_owned(),
        });
    }
}

fn validate_pose(path: &str, pose: Pose, errors: &mut Vec<ValidationError>) {
    finite(&format!("{path}.xyz"), pose.xyz, errors);
    finite(&format!("{path}.rpy"), pose.rpy, errors);
}

fn finite<const N: usize>(path: &str, values: [f64; N], errors: &mut Vec<ValidationError>) {
    if !values.into_iter().all(f64::is_finite) {
        errors.push(ValidationError::NonFinite {
            path: path.to_owned(),
        });
    }
}

fn validate_geometry(path: &str, geometry: &Geometry, errors: &mut Vec<ValidationError>) {
    let dimensions: &[f64] = match geometry {
        Geometry::Box { size } => size,
        Geometry::Cylinder { radius, length } | Geometry::Capsule { radius, length } => {
            &[*radius, *length]
        }
        Geometry::Sphere { radius } => &[*radius],
        Geometry::Mesh { path: mesh, scale } => {
            if mesh.is_absolute()
                || mesh
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
                || mesh.extension().and_then(|extension| extension.to_str()) != Some("glb")
            {
                errors.push(ValidationError::InvalidMeshPath {
                    path: path.to_owned(),
                });
            }
            scale.as_ref().map_or(&[], |values| values.as_slice())
        }
    };
    if dimensions.iter().any(|value| !value.is_finite()) {
        errors.push(ValidationError::NonFinite {
            path: path.to_owned(),
        });
    } else if dimensions.iter().any(|value| *value <= 0.0) {
        errors.push(ValidationError::InvalidDimensions {
            path: path.to_owned(),
        });
    }
}
