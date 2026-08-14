fn finite(value: f64) -> bool {
    value.is_finite()
}

/// A finite per-joint position/velocity sample.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "JointStateWire")]
pub struct JointState {
    pub position_rad: f64,
    pub velocity_radps: f64,
    pub effort_nm: Option<f64>,
}

impl JointState {
    pub fn try_new(
        position_rad: f64,
        velocity_radps: f64,
        effort_nm: Option<f64>,
    ) -> Result<Self, JointStateError> {
        if !finite(position_rad) || !finite(velocity_radps) || !effort_nm.is_none_or(finite) {
            return Err(JointStateError::NonFinite);
        }
        Ok(Self {
            position_rad,
            velocity_radps,
            effort_nm,
        })
    }

    #[must_use]
    pub const fn position_rad(&self) -> f64 {
        self.position_rad
    }

    #[must_use]
    pub const fn velocity_radps(&self) -> f64 {
        self.velocity_radps
    }

    #[must_use]
    pub const fn effort_nm(&self) -> Option<f64> {
        self.effort_nm
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JointStateError {
    NonFinite,
}

impl std::fmt::Display for JointStateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("joint state fields must be finite")
    }
}
impl std::error::Error for JointStateError {}

#[doc(hidden)]
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct JointStateWire {
    position_rad: f64,
    velocity_radps: f64,
    effort_nm: Option<f64>,
}

impl TryFrom<JointStateWire> for JointState {
    type Error = JointStateError;
    fn try_from(value: JointStateWire) -> Result<Self, Self::Error> {
        Self::try_new(value.position_rad, value.velocity_radps, value.effort_nm)
    }
}

phoxal_macros::protocol_fragment! {
    path robot / joint(joint);

    topic state: Event<JointState>;
}
