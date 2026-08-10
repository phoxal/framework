const MAX_FRAME_ID_LEN: usize = 128;

fn finite(value: f64) -> bool {
    value.is_finite()
}

fn valid_frame_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_FRAME_ID_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "FrameTransformWire")]
pub struct FrameTransform {
    pub parent_frame_id: String,
    pub child_frame_id: String,
    pub translation_m: [f64; 3],
    pub rotation_quat_xyzw: [f64; 4],
    pub stamp: Option<::phoxal_bus::RobotInstant>,
}

impl FrameTransform {
    pub fn try_new(
        parent_frame_id: String,
        child_frame_id: String,
        translation_m: [f64; 3],
        rotation_quat_xyzw: [f64; 4],
        stamp: Option<::phoxal_bus::RobotInstant>,
    ) -> Result<Self, FrameTransformError> {
        if !valid_frame_id(&parent_frame_id) || !valid_frame_id(&child_frame_id) {
            return Err(FrameTransformError::InvalidFrameId);
        }
        if !translation_m.into_iter().all(finite) {
            return Err(FrameTransformError::NonFiniteTranslation);
        }
        if !rotation_quat_xyzw.into_iter().all(finite) {
            return Err(FrameTransformError::NonFiniteQuaternion);
        }
        let norm = rotation_quat_xyzw
            .into_iter()
            .map(|value| value * value)
            .sum::<f64>()
            .sqrt();
        if !norm.is_finite() || norm <= f64::EPSILON || (norm - 1.0).abs() > 1.0e-6 {
            return Err(FrameTransformError::QuaternionNotNormalized);
        }
        Ok(Self {
            parent_frame_id,
            child_frame_id,
            translation_m,
            rotation_quat_xyzw,
            stamp,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameTransformError {
    InvalidFrameId,
    NonFiniteTranslation,
    NonFiniteQuaternion,
    QuaternionNotNormalized,
}

impl std::fmt::Display for FrameTransformError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidFrameId => "frame ids must be non-empty bounded key segments",
            Self::NonFiniteTranslation => "frame translation must be finite",
            Self::NonFiniteQuaternion => "frame quaternion must be finite",
            Self::QuaternionNotNormalized => "frame quaternion must be normalized and nonzero",
        })
    }
}
impl std::error::Error for FrameTransformError {}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StaticTransforms {
    pub transforms: Vec<FrameTransform>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tree {
    pub transforms: Vec<FrameTransform>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "LookupRequestWire")]
pub struct LookupRequest {
    pub target_frame_id: String,
    pub source_frame_id: String,
    pub at: Option<::phoxal_bus::RobotInstant>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LookupResponse {
    pub transform: Option<FrameTransform>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FrameTransformWire {
    parent_frame_id: String,
    child_frame_id: String,
    translation_m: [f64; 3],
    rotation_quat_xyzw: [f64; 4],
    stamp: Option<::phoxal_bus::RobotInstant>,
}

impl TryFrom<FrameTransformWire> for FrameTransform {
    type Error = FrameTransformError;
    fn try_from(value: FrameTransformWire) -> Result<Self, Self::Error> {
        Self::try_new(
            value.parent_frame_id,
            value.child_frame_id,
            value.translation_m,
            value.rotation_quat_xyzw,
            value.stamp,
        )
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LookupRequestWire {
    target_frame_id: String,
    source_frame_id: String,
    at: Option<::phoxal_bus::RobotInstant>,
}

impl TryFrom<LookupRequestWire> for LookupRequest {
    type Error = FrameTransformError;
    fn try_from(value: LookupRequestWire) -> Result<Self, Self::Error> {
        if !valid_frame_id(&value.target_frame_id) || !valid_frame_id(&value.source_frame_id) {
            return Err(FrameTransformError::InvalidFrameId);
        }
        Ok(Self {
            target_frame_id: value.target_frame_id,
            source_frame_id: value.source_frame_id,
            at: value.at,
        })
    }
}

phoxal_macros::phoxal_api_fragment! {
    path frame;

    version v0_1;

    topic tree: State<Tree>;
    topic static_transforms: State<StaticTransforms>;
    query lookup: LookupRequest => LookupResponse;
}
