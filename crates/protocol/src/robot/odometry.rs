fn finite(value: f64) -> bool {
    value.is_finite()
}

fn finite_f32(value: f32) -> bool {
    value.is_finite()
}

fn canonical_yaw(value: f64) -> bool {
    value.is_finite() && (-std::f64::consts::PI..=std::f64::consts::PI).contains(&value)
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "StateWire")]
pub struct State {
    pub x_m: f64,
    pub y_m: f64,
    pub yaw_rad: f64,
    pub linear_x_mps: f32,
    pub angular_z_radps: f32,
}

impl State {
    pub fn try_new(
        x_m: f64,
        y_m: f64,
        yaw_rad: f64,
        linear_x_mps: f32,
        angular_z_radps: f32,
    ) -> Result<Self, &'static str> {
        if !finite(x_m)
            || !finite(y_m)
            || !canonical_yaw(yaw_rad)
            || !finite_f32(linear_x_mps)
            || !finite_f32(angular_z_radps)
        {
            return Err("odometry state must contain finite values and canonical yaw");
        }
        Ok(Self {
            x_m,
            y_m,
            yaw_rad,
            linear_x_mps,
            angular_z_radps,
        })
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StateWire {
    x_m: f64,
    y_m: f64,
    yaw_rad: f64,
    linear_x_mps: f32,
    angular_z_radps: f32,
}

impl TryFrom<StateWire> for State {
    type Error = &'static str;
    fn try_from(value: StateWire) -> Result<Self, Self::Error> {
        Self::try_new(
            value.x_m,
            value.y_m,
            value.yaw_rad,
            value.linear_x_mps,
            value.angular_z_radps,
        )
    }
}
