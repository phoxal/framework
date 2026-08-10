/// A published map revision marker.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Revision {
    pub revision: u64,
    pub resolution_m: f32,
}

/// Request a rectangular submap window (map-frame metres).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SubmapRequest {
    pub min_x_m: f64,
    pub min_y_m: f64,
    pub max_x_m: f64,
    pub max_y_m: f64,
}

/// A finite world-space point used as the cell origin and pose
/// translation in a self-describing grid response.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "crate::api::robot::map::GridPointWire")]
pub struct Point {
    pub x_m: f64,
    pub y_m: f64,
}

/// The map-frame pose of the grid's reference origin.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "crate::api::robot::map::GridPoseWire")]
pub struct Pose {
    pub x_m: f64,
    pub y_m: f64,
    pub yaw_rad: f64,
}

/// Requested and covered map-frame bounds.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "crate::api::robot::map::GridBoundsWire")]
pub struct Bounds {
    pub min_x_m: f64,
    pub min_y_m: f64,
    pub max_x_m: f64,
    pub max_y_m: f64,
}

/// Occupancy has a closed wire domain. Unknown is not treated as
/// free by safety or navigation.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Occupancy {
    Free,
    Occupied,
    Unknown,
}

/// A revisioned map window whose origin, frame, extent and bounds
/// travel with the cells themselves.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "crate::api::robot::map::GridWindowWire")]
pub struct GridWindow {
    pub frame_id: String,
    pub origin_pose: Pose,
    pub cell_origin: Point,
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_finite_positive_resolution")]
    pub resolution_m: f32,
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_nonzero_map_dimension")]
    pub width: u32,
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_nonzero_map_dimension")]
    pub height: u32,
    pub cells: Vec<Occupancy>,
    pub revision: u64,
    pub requested: Bounds,
    pub covered: Bounds,
}

/// A query either returns a complete window, a clipped window
/// with explicit requested/covered bounds, or an explicit
/// out-of-bounds result. A responder may not silently substitute
/// a different extent for what was requested.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SubmapResponse {
    Window(GridWindow),
    Partial {
        window: GridWindow,
    },
    OutOfBounds {
        requested: Bounds,
        #[serde(deserialize_with = "crate::api::robot::map::deserialize_nonempty_frame_id")]
        frame_id: String,
        revision: u64,
    },
}

/// Deserialize a map scalar that participates in a world-space bound.
pub(crate) fn deserialize_finite_map_scalar<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <f64 as serde::Deserialize>::deserialize(deserializer)?;
    value
        .is_finite()
        .then_some(value)
        .ok_or_else(|| serde::de::Error::custom("map coordinate must be finite"))
}

/// A map window dimension is part of a checked `width * height` shape, so zero
/// is not a valid wire value even before the cell vector is considered.
pub(crate) fn deserialize_nonzero_map_dimension<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <u32 as serde::Deserialize>::deserialize(deserializer)?;
    (value != 0)
        .then_some(value)
        .ok_or_else(|| serde::de::Error::custom("map window dimensions must be nonzero"))
}

/// Grid resolution is a physical scalar rather than an arbitrary float.
pub(crate) fn deserialize_finite_positive_resolution<'de, D>(
    deserializer: D,
) -> Result<f32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <f32 as serde::Deserialize>::deserialize(deserializer)?;
    (value.is_finite() && value > 0.0)
        .then_some(value)
        .ok_or_else(|| serde::de::Error::custom("map resolution must be finite and positive"))
}

/// A frame name is part of the map identity and may not be absent or blank.
pub(crate) fn deserialize_nonempty_frame_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <String as serde::Deserialize>::deserialize(deserializer)?;
    (!value.trim().is_empty())
        .then_some(value)
        .ok_or_else(|| serde::de::Error::custom("map frame id must not be empty"))
}

#[doc(hidden)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GridBoundsWire {
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_finite_map_scalar")]
    pub min_x_m: f64,
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_finite_map_scalar")]
    pub min_y_m: f64,
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_finite_map_scalar")]
    pub max_x_m: f64,
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_finite_map_scalar")]
    pub max_y_m: f64,
}

#[doc(hidden)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GridPoseWire {
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_finite_map_scalar")]
    pub x_m: f64,
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_finite_map_scalar")]
    pub y_m: f64,
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_finite_map_scalar")]
    pub yaw_rad: f64,
}

#[doc(hidden)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GridPointWire {
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_finite_map_scalar")]
    pub x_m: f64,
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_finite_map_scalar")]
    pub y_m: f64,
}

#[doc(hidden)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GridWindowWire {
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_nonempty_frame_id")]
    pub frame_id: String,
    pub origin_pose: crate::api::robot::map::Pose,
    pub cell_origin: crate::api::robot::map::Point,
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_finite_positive_resolution")]
    pub resolution_m: f32,
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_nonzero_map_dimension")]
    pub width: u32,
    #[serde(deserialize_with = "crate::api::robot::map::deserialize_nonzero_map_dimension")]
    pub height: u32,
    pub cells: Vec<crate::api::robot::map::Occupancy>,
    pub revision: u64,
    pub requested: crate::api::robot::map::Bounds,
    pub covered: crate::api::robot::map::Bounds,
}

#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct GridWireError(pub &'static str);

impl std::fmt::Display for GridWireError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for GridWireError {}

impl TryFrom<GridBoundsWire> for crate::api::robot::map::Bounds {
    type Error = GridWireError;

    fn try_from(value: GridBoundsWire) -> Result<Self, Self::Error> {
        if !(value.min_x_m < value.max_x_m && value.min_y_m < value.max_y_m) {
            return Err(GridWireError("map bounds must have positive extent"));
        }
        Ok(Self {
            min_x_m: value.min_x_m,
            min_y_m: value.min_y_m,
            max_x_m: value.max_x_m,
            max_y_m: value.max_y_m,
        })
    }
}

impl TryFrom<GridPoseWire> for crate::api::robot::map::Pose {
    type Error = GridWireError;

    fn try_from(value: GridPoseWire) -> Result<Self, Self::Error> {
        Ok(Self {
            x_m: value.x_m,
            y_m: value.y_m,
            yaw_rad: value.yaw_rad,
        })
    }
}

impl TryFrom<GridPointWire> for crate::api::robot::map::Point {
    type Error = GridWireError;

    fn try_from(value: GridPointWire) -> Result<Self, Self::Error> {
        Ok(Self {
            x_m: value.x_m,
            y_m: value.y_m,
        })
    }
}

impl TryFrom<GridWindowWire> for crate::api::robot::map::GridWindow {
    type Error = GridWireError;

    fn try_from(value: GridWindowWire) -> Result<Self, Self::Error> {
        let expected = usize::try_from(value.width)
            .ok()
            .and_then(|width| {
                usize::try_from(value.height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .ok_or(GridWireError("map grid dimensions overflow"))?;
        if value.cells.len() != expected {
            return Err(GridWireError(
                "map grid cell shape does not match dimensions",
            ));
        }
        if !(value.covered.min_x_m >= value.requested.min_x_m
            && value.covered.min_y_m >= value.requested.min_y_m
            && value.covered.max_x_m <= value.requested.max_x_m
            && value.covered.max_y_m <= value.requested.max_y_m)
        {
            return Err(GridWireError(
                "map covered bounds must be contained in requested bounds",
            ));
        }
        let epsilon = f64::from(value.resolution_m) * 1.0e-6;
        if (value.cell_origin.x_m - value.covered.min_x_m).abs() > epsilon
            || (value.cell_origin.y_m - value.covered.min_y_m).abs() > epsilon
        {
            return Err(GridWireError(
                "map cell origin must equal the covered lower corner",
            ));
        }
        if (value.origin_pose.x_m - value.cell_origin.x_m).abs() > epsilon
            || (value.origin_pose.y_m - value.cell_origin.y_m).abs() > epsilon
        {
            return Err(GridWireError(
                "map origin pose translation must equal the cell origin",
            ));
        }
        let covered_width = f64::from(value.width) * f64::from(value.resolution_m);
        let covered_height = f64::from(value.height) * f64::from(value.resolution_m);
        if (value.covered.max_x_m - value.covered.min_x_m - covered_width).abs() > epsilon
            || (value.covered.max_y_m - value.covered.min_y_m - covered_height).abs() > epsilon
        {
            return Err(GridWireError("map covered bounds do not match grid extent"));
        }
        Ok(Self {
            frame_id: value.frame_id,
            origin_pose: value.origin_pose,
            cell_origin: value.cell_origin,
            resolution_m: value.resolution_m,
            width: value.width,
            height: value.height,
            cells: value.cells,
            revision: value.revision,
            requested: value.requested,
            covered: value.covered,
        })
    }
}

phoxal_macros::phoxal_api_fragment! {
    path robot / map;

    topic revision: State<Revision>;
    query submap: SubmapRequest => SubmapResponse;
}
