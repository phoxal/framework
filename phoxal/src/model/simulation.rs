//! Canonical simulation facts used after authored documents are loaded.
//!
//! A simulation describes how one component type behaves in a simulated world.
//! It never introduces capabilities of its own: every entry here must match a
//! capability the component type already declares, of the same
//! [`CapabilityKind`].

use std::collections::BTreeMap;

use crate::component::capability::CapabilityKind;
use crate::identity::{CapabilityId, LinkId};

/// The simulated behaviour of one component type.
#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Simulation {
    capabilities: BTreeMap<CapabilityId, Capability>,
    links: BTreeMap<LinkId, Link>,
}

/// The simulated properties of one component-local link.
#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq,
)]
#[serde(deny_unknown_fields)]
pub struct Link {
    contact_material: Option<String>,
}

impl Simulation {
    pub(crate) fn new(
        capabilities: BTreeMap<CapabilityId, Capability>,
        links: BTreeMap<LinkId, Option<String>>,
    ) -> Self {
        Self {
            capabilities,
            links: links
                .into_iter()
                .map(|(id, contact_material)| (id, Link { contact_material }))
                .collect(),
        }
    }

    /// Every simulated capability, ordered by capability id.
    pub fn capabilities(&self) -> impl ExactSizeIterator<Item = (&CapabilityId, &Capability)> {
        self.capabilities.iter()
    }

    /// The named simulated capability, if this simulation models it.
    #[must_use]
    pub fn capability(&self, id: &str) -> Option<&Capability> {
        self.capabilities.get(id)
    }

    /// Every simulated link, ordered by link id.
    pub fn links(&self) -> impl ExactSizeIterator<Item = (&LinkId, &Link)> {
        self.links.iter()
    }
}

impl Link {
    /// The named contact material, when the world defines one for this link.
    #[must_use]
    pub fn contact_material(&self) -> Option<&str> {
        self.contact_material.as_deref()
    }
}

/// Canonical simulation parameters normalized from a versioned `simulation.yaml`.
#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Capability {
    Motor(Motor),
    Encoder(Encoder),
    Accelerometer(Accelerometer),
    Gyroscope(Gyroscope),
    Magnetometer(Magnetometer),
    Imu(Imu),
    Gnss(Gnss),
    Camera(Camera),
    Depth(Depth),
    Range(Range),
    Lidar(Lidar),
    Mmwave(Mmwave),
    Microphone(Microphone),
    Speaker,
    Battery,
    Led,
    EmergencyStop,
}

impl Capability {
    /// The device kind this simulation models.
    #[must_use]
    pub const fn kind(&self) -> CapabilityKind {
        match self {
            Self::Motor(_) => CapabilityKind::Motor,
            Self::Encoder(_) => CapabilityKind::Encoder,
            Self::Accelerometer(_) => CapabilityKind::Accelerometer,
            Self::Gyroscope(_) => CapabilityKind::Gyroscope,
            Self::Magnetometer(_) => CapabilityKind::Magnetometer,
            Self::Imu(_) => CapabilityKind::Imu,
            Self::Gnss(_) => CapabilityKind::Gnss,
            Self::Camera(_) => CapabilityKind::Camera,
            Self::Depth(_) => CapabilityKind::Depth,
            Self::Range(_) => CapabilityKind::Range,
            Self::Lidar(_) => CapabilityKind::Lidar,
            Self::Mmwave(_) => CapabilityKind::Mmwave,
            Self::Microphone(_) => CapabilityKind::Microphone,
            Self::Speaker => CapabilityKind::Speaker,
            Self::Battery => CapabilityKind::Battery,
            Self::Led => CapabilityKind::Led,
            Self::EmergencyStop => CapabilityKind::EmergencyStop,
        }
    }
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ActuatorType {
    #[default]
    Velocity,
    Position,
    Torque,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CameraProjection {
    Planar,
    Cylindrical,
    Spherical,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    PartialEq,
    Default,
)]
#[serde(deny_unknown_fields)]
pub struct Motor {
    pub actuator_type: ActuatorType,
    pub acceleration_radps2: Option<f64>,
    pub control_pid: Option<Vec<f64>>,
    pub sampling_period_torque_hz: Option<f64>,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Default,
)]
#[serde(deny_unknown_fields)]
pub struct Encoder {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub noise: Option<f64>,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    PartialEq,
    Default,
)]
#[serde(deny_unknown_fields)]
pub struct Accelerometer {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    PartialEq,
    Default,
)]
#[serde(deny_unknown_fields)]
pub struct Gyroscope {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    PartialEq,
    Default,
)]
#[serde(deny_unknown_fields)]
pub struct Magnetometer {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Default,
)]
#[serde(deny_unknown_fields)]
pub struct Imu {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub noise: Option<f64>,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Default,
)]
#[serde(deny_unknown_fields)]
pub struct Gnss {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub accuracy: Option<f64>,
    pub noise_correlation: Option<f64>,
    pub speed_resolution: Option<f64>,
    pub speed_noise: Option<f64>,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    PartialEq,
    Default,
)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    pub sampling_period_hz: f64,
    pub projection: Option<CameraProjection>,
    pub near: Option<f64>,
    pub far: Option<f64>,
    pub exposure: Option<f64>,
    pub anti_aliasing: Option<bool>,
    pub ambient_occlusion_radius: Option<f64>,
    pub bloom_threshold: Option<f64>,
    pub noise: Option<f64>,
    pub motion_blur: Option<f64>,
    pub noise_mask_url: Option<String>,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Default,
)]
#[serde(deny_unknown_fields)]
pub struct Depth {
    pub sampling_period_hz: f64,
    pub noise: Option<f64>,
    pub resolution: Option<f64>,
    pub motion_blur: Option<f64>,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Default,
)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub sampling_period_hz: f64,
    pub noise: Option<f64>,
    pub resolution: Option<f64>,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Default,
)]
#[serde(deny_unknown_fields)]
pub struct Lidar {
    pub sampling_period_hz: f64,
    pub noise: Option<f64>,
    pub resolution: Option<f64>,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    PartialEq,
    Default,
)]
#[serde(deny_unknown_fields)]
pub struct Mmwave {
    pub sampling_period_hz: f64,
    pub noise: Option<f64>,
    pub resolution: Option<f64>,
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Default,
)]
#[serde(deny_unknown_fields)]
pub struct Microphone {
    pub sampling_period_hz: f64,
    pub aperture: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_simulation_keeps_its_bare_string_map_keys_on_the_wire() {
        // Typing the map keys must not turn them into objects: the wire form
        // is still a plain id-to-record map.
        let simulation = Simulation::new(
            [(
                CapabilityId::new("spin").expect("a normalized capability id"),
                Capability::Speaker,
            )]
            .into_iter()
            .collect(),
            [(LinkId::new("body"), Some("rubber".to_string()))]
                .into_iter()
                .collect(),
        );
        let json = serde_json::to_string(&simulation).expect("the simulation serializes");
        assert_eq!(
            json,
            r#"{"capabilities":{"spin":{"kind":"speaker"}},"links":{"body":{"contact_material":"rubber"}}}"#
        );
        assert_eq!(
            serde_json::from_str::<Simulation>(&json).expect("the simulation round-trips"),
            simulation
        );
    }

    #[test]
    fn a_simulated_capability_reports_the_kind_it_models() {
        assert_eq!(Capability::Led.kind(), CapabilityKind::Led);
        assert_eq!(
            Capability::Encoder(Encoder::default()).kind(),
            CapabilityKind::Encoder
        );
    }
}
