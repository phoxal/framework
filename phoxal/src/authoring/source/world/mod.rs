//! Versioned authored `world.yaml` documents.

pub mod v0;

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::authoring::source::document::{Document, DocumentKind, Origin, SourceError};
use crate::authoring::source::{Violations, strict_yaml};

/// A versioned authored world document selected by its schema tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "schema")]
pub enum Manifest {
    #[serde(rename = "phoxal/world/v0")]
    V0(v0::Manifest),
}

impl Document for Manifest {
    const KIND: DocumentKind = DocumentKind::World;

    fn check(&self) -> Result<(), Violations> {
        let Self::V0(body) = self;
        body.validate().map_err(Violations::World)
    }

    fn precheck(text: &str, origin: &Origin) -> Result<(), SourceError> {
        strict_yaml::check(text).map_err(|source| SourceError::StrictYaml {
            kind: Self::KIND,
            origin: origin.clone(),
            source,
        })
    }
}

impl Manifest {
    /// Parse and validate one complete world document from text.
    pub fn parse(text: &str) -> Result<Self, SourceError> {
        Self::read_text(text, Origin::Text)
    }

    /// Load one explicit `world.yaml` file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SourceError> {
        Self::read_path(path.as_ref())
    }

    /// Write the document into `directory` as `world.yaml`.
    pub fn write_to_dir(&self, directory: impl AsRef<Path>) -> Result<(), SourceError> {
        self.write_dir(directory.as_ref())
    }

    pub(crate) fn normalize(self) -> crate::authoring::normalized::World {
        use crate::authoring::normalized::{World, WorldAsset, WorldEntity};
        let Self::V0(body) = self;
        World {
            id: body.world.id,
            time_step_ms: body.world.time_step_ms,
            gravity_mps2: body.world.gravity_mps2.map(normalize_float),
            assets: body
                .assets
                .into_iter()
                .map(|(name, asset)| {
                    let geometry = normalize_geometry(asset.geometry);
                    let collision = asset
                        .collision
                        .map(normalize_geometry)
                        .unwrap_or_else(|| geometry.clone());
                    (
                        name,
                        WorldAsset {
                            geometry,
                            collision,
                        },
                    )
                })
                .collect(),
            spawn_points: body
                .world
                .spawn_points
                .into_iter()
                .map(|(name, pose)| (name, normalize_pose(pose)))
                .collect(),
            entities: body
                .world
                .entities
                .into_iter()
                .map(|(name, entity)| {
                    (
                        name,
                        WorldEntity {
                            asset: entity.asset,
                            instances: entity
                                .instances
                                .into_iter()
                                .map(|instance| normalize_pose(instance.pose))
                                .collect(),
                        },
                    )
                })
                .collect(),
        }
    }
}

fn normalize_float(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

fn normalize_pose(pose: v0::Pose) -> crate::model::structure::Pose {
    crate::model::structure::Pose::from_validated_parts(
        pose.xyz.map(normalize_float),
        pose.rpy.map(normalize_float),
    )
}

fn normalize_geometry(geometry: v0::Geometry) -> crate::authoring::normalized::WorldGeometry {
    use crate::authoring::normalized::WorldGeometry;
    use crate::model::geometry::Geometry;
    WorldGeometry::Primitive(match geometry {
        v0::Geometry::Box { size } => Geometry::Box {
            size: size.map(normalize_float),
        },
        v0::Geometry::Cylinder { radius, length } => Geometry::Cylinder { radius, length },
        v0::Geometry::Capsule { radius, length } => Geometry::Capsule { radius, length },
        v0::Geometry::Sphere { radius } => Geometry::Sphere { radius },
        v0::Geometry::Mesh { path, scale } => {
            return WorldGeometry::Mesh {
                path,
                scale: scale.map(|scale| scale.map(normalize_float)),
            };
        }
    })
}

#[cfg(test)]
mod tests {
    use super::Manifest;

    const WORLD: &str = r#"
schema: phoxal/world/v0
assets:
  floor:
    geometry: { kind: box, size: [10.0, 10.0, 0.1] }
world:
  id: warehouse
  time_step_ms: 12
  gravity_mps2: [0.0, 0.0, -9.81]
  spawn_points:
    loading-bay: { xyz: [0.0, 0.0, 0.0], rpy: [0.0, 0.0, 0.0] }
  entities:
    floor:
      asset: floor
      instances:
        - pose: { xyz: [0.0, 0.0, -0.05], rpy: [0.0, 0.0, 0.0] }
"#;

    #[test]
    fn world_round_trips_through_its_exact_schema() -> anyhow::Result<()> {
        let parsed = Manifest::parse(WORLD)?;
        let directory = tempfile::tempdir()?;
        parsed.write_to_dir(directory.path())?;
        assert_eq!(Manifest::load(directory.path())?, parsed);
        Ok(())
    }

    #[test]
    fn world_rejects_single_pose_and_unknown_fields() {
        let invalid = WORLD.replace(
            "instances:\n        - pose: { xyz: [0.0, 0.0, -0.05], rpy: [0.0, 0.0, 0.0] }",
            "pose: { xyz: [0.0, 0.0, -0.05], rpy: [0.0, 0.0, 0.0] }",
        );
        assert!(Manifest::parse(&invalid).is_err());
    }
}
