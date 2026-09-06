//! Canonical geometry shared by robot structures and compiled worlds.

use serde::{Deserialize, Serialize};

use crate::model::asset::AssetId;

/// Complete canonical geometry vocabulary.
#[derive(phoxal_macros::DescribeWire, Clone, Debug, Deserialize, Serialize, PartialEq)]
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
        #[serde(rename = "filename")]
        asset: AssetId,
        scale: Option<[f64; 3]>,
    },
}

impl Geometry {
    /// The bundled asset this geometry references, when it is a mesh.
    #[must_use]
    pub fn asset_id(&self) -> Option<&AssetId> {
        match self {
            Self::Mesh { asset, .. } => Some(asset),
            _ => None,
        }
    }

    /// Whether every authored dimension is finite and strictly positive.
    #[must_use]
    pub fn has_valid_dimensions(&self) -> bool {
        let dimensions: &[f64] = match self {
            Self::Box { size } => size,
            Self::Cylinder { radius, length } | Self::Capsule { radius, length } => {
                &[*radius, *length]
            }
            Self::Sphere { radius } => &[*radius],
            Self::Mesh { scale, .. } => scale.as_ref().map_or(&[], |values| values.as_slice()),
        };
        dimensions
            .iter()
            .all(|value| value.is_finite() && *value > 0.0)
    }
}
