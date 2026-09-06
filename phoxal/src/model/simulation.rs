//! Canonical simulation facts used after authored documents are loaded.
//!
//! A simulation describes how one component type behaves in a simulated world.
//! It never introduces capabilities of its own: every entry here must match a
//! capability the component type already declares, of the same
//! [`CapabilityKind`].

use std::collections::{BTreeMap, BTreeSet};

use crate::model::asset::AssetId;
use crate::model::component::capability::CapabilityKind;
use crate::model::identity::{CapabilityId, CapabilityRef, ComponentInstanceId, LinkId};
use crate::model::robot::Robot;

/// The backend-neutral, all-or-nothing simulation plan for one compiled robot.
///
/// Deriving this value proves that every mounted component has simulation data,
/// every declared typed capability has exactly one matching simulation entry,
/// and every physical driver participant has exactly one adapter substitution.
/// It also records the complete asset closure that an adapter must make
/// available before mutating its native world.
///
/// Native device naming, supported simulator facts, geometry conversion, and
/// backend-version checks remain adapter-owned admission work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullSimulationPlan {
    substitutions: Vec<DriverSubstitution>,
    capabilities: Vec<CapabilityRef>,
    required_assets: Vec<AssetId>,
}

/// One physical driver participant that a full-simulation adapter must replace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverSubstitution {
    participant: ComponentInstanceId,
    capabilities: Vec<CapabilityRef>,
}

/// A locally provable reason why a compiled robot cannot enter full simulation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FullSimulationError {
    #[error("component '{component}' has no compiled simulation data")]
    MissingSimulation { component: ComponentInstanceId },

    #[error("component capability '{capability}' has no simulation binding")]
    MissingCapability { capability: CapabilityRef },

    #[error("component simulation contains unknown capability '{capability}'")]
    ExtraCapability { capability: CapabilityRef },

    #[error("component '{component}' simulation references unknown structural link '{link}'")]
    UnknownLink {
        component: ComponentInstanceId,
        link: LinkId,
    },

    #[error(
        "component capability '{capability}' is {declared}, but its simulation binding is {simulated}"
    )]
    CapabilityKindMismatch {
        capability: CapabilityRef,
        declared: CapabilityKind,
        simulated: CapabilityKind,
    },

    #[error("simulation capability '{capability}' references invalid asset id '{value}'")]
    InvalidAssetId {
        capability: CapabilityRef,
        value: String,
    },

    #[error("required simulation asset '{asset}' is absent from the compiled bundle")]
    MissingAsset { asset: AssetId },

    #[error("required simulation asset '{asset}' is empty")]
    EmptyAsset { asset: AssetId },
}

impl FullSimulationPlan {
    /// Derive the complete backend-neutral plan from one canonical robot.
    ///
    /// # Errors
    ///
    /// Returns [`FullSimulationError`] when a component has no simulation,
    /// capability coverage is partial or inconsistent, or a simulator asset
    /// reference is not a canonical [`AssetId`].
    pub fn derive(robot: &Robot) -> Result<Self, FullSimulationError> {
        let mut substitutions = Vec::new();
        let mut capabilities = Vec::new();
        let mut required_assets = robot
            .structure()
            .asset_ids()
            .cloned()
            .collect::<BTreeSet<_>>();

        for component in robot.components() {
            let component_id = component.id();
            let declared = component.component_type();
            let simulation =
                component
                    .simulation()
                    .ok_or_else(|| FullSimulationError::MissingSimulation {
                        component: component_id.clone(),
                    })?;
            required_assets.extend(declared.structure().asset_ids().cloned());

            for (link, _) in simulation.links() {
                if declared.structure().link(link.as_str()).is_none() {
                    return Err(FullSimulationError::UnknownLink {
                        component: component_id.clone(),
                        link: link.clone(),
                    });
                }
            }

            let mut substituted_capabilities = Vec::new();
            for (capability_id, capability) in declared.capabilities() {
                let reference = CapabilityRef::new(component_id.clone(), capability_id.clone());
                let simulated = simulation
                    .capability(capability_id.as_str())
                    .ok_or_else(|| FullSimulationError::MissingCapability {
                        capability: reference.clone(),
                    })?;
                if capability.kind() != simulated.kind() {
                    return Err(FullSimulationError::CapabilityKindMismatch {
                        capability: reference,
                        declared: capability.kind(),
                        simulated: simulated.kind(),
                    });
                }
                collect_capability_assets(simulated, &reference, &mut required_assets)?;
                substituted_capabilities.push(reference.clone());
                capabilities.push(reference);
            }
            for (capability_id, _) in simulation.capabilities() {
                if declared.capability(capability_id.as_str()).is_none() {
                    return Err(FullSimulationError::ExtraCapability {
                        capability: CapabilityRef::new(component_id.clone(), capability_id.clone()),
                    });
                }
            }
            if component.instance().driver().is_some() {
                substitutions.push(DriverSubstitution {
                    participant: component_id.clone(),
                    capabilities: substituted_capabilities,
                });
            }
        }

        Ok(Self {
            substitutions,
            capabilities,
            required_assets: required_assets.into_iter().collect(),
        })
    }

    /// Every omitted physical driver, ordered by participant identity.
    pub fn substitutions(&self) -> impl ExactSizeIterator<Item = &DriverSubstitution> {
        self.substitutions.iter()
    }

    /// Every typed capability that the adapter must bind, ordered by component
    /// and capability identity.
    pub fn capabilities(&self) -> impl ExactSizeIterator<Item = &CapabilityRef> {
        self.capabilities.iter()
    }

    /// Every geometry or simulator asset needed by this robot, ordered by id.
    pub fn required_assets(&self) -> impl ExactSizeIterator<Item = &AssetId> {
        self.required_assets.iter()
    }

    /// Prove that every required asset is present and non-empty.
    ///
    /// The callback returns the asset byte length, or `None` when the closed
    /// bundle does not contain the requested id.
    ///
    /// # Errors
    ///
    /// Returns [`FullSimulationError::MissingAsset`] or
    /// [`FullSimulationError::EmptyAsset`] for the first incomplete asset.
    pub fn validate_assets(
        &self,
        mut asset_len: impl FnMut(&AssetId) -> Option<usize>,
    ) -> Result<(), FullSimulationError> {
        for asset in &self.required_assets {
            match asset_len(asset) {
                None => {
                    return Err(FullSimulationError::MissingAsset {
                        asset: asset.clone(),
                    });
                }
                Some(0) => {
                    return Err(FullSimulationError::EmptyAsset {
                        asset: asset.clone(),
                    });
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

impl DriverSubstitution {
    /// The participant id omitted from a full-simulation execution.
    #[must_use]
    pub const fn participant(&self) -> &ComponentInstanceId {
        &self.participant
    }

    /// Every typed capability supplied by this participant's adapter replacement.
    pub fn capabilities(&self) -> impl ExactSizeIterator<Item = &CapabilityRef> {
        self.capabilities.iter()
    }
}

fn collect_capability_assets(
    capability: &Capability,
    reference: &CapabilityRef,
    assets: &mut BTreeSet<AssetId>,
) -> Result<(), FullSimulationError> {
    if let Capability::Camera(camera) = capability
        && let Some(value) = &camera.noise_mask_url
    {
        let asset =
            AssetId::new(value.clone()).map_err(|_| FullSimulationError::InvalidAssetId {
                capability: reference.clone(),
                value: value.clone(),
            })?;
        assets.insert(asset);
    }
    Ok(())
}

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
    schemars::JsonSchema,
)]
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
    schemars::JsonSchema,
)]
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
    use crate::model::builder::RobotBuilder;
    use crate::model::connection::{Connection, Serial};

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

    #[test]
    fn full_simulation_rejects_missing_and_partial_component_mappings() {
        let missing = RobotBuilder::new("missing")
            .component_type("wheel", |wheel| wheel.encoder("turns", "axle"))
            .component("left", "wheel")
            .build()
            .expect("the hardware model is valid without simulation data");
        assert!(matches!(
            FullSimulationPlan::derive(&missing),
            Err(FullSimulationError::MissingSimulation { .. })
        ));

        let partial = RobotBuilder::new("partial")
            .component_type("wheel", |wheel| {
                wheel
                    .motor("spin", "axle")
                    .encoder("turns", "axle")
                    .simulated("spin", Capability::Motor(Motor::default()))
            })
            .component("left", "wheel")
            .build()
            .expect("the canonical model permits a partial simulation mapping");
        assert!(matches!(
            FullSimulationPlan::derive(&partial),
            Err(FullSimulationError::MissingCapability { capability })
                if capability.to_string() == "left.turns"
        ));
    }

    #[test]
    fn full_simulation_plans_each_driver_once_and_closes_simulator_assets() {
        let mask = "components/camera/meshes/noise-mask.png";
        let robot = RobotBuilder::new("camera-bot")
            .component_type("camera", |camera| {
                camera.camera("image", "lens").simulated(
                    "image",
                    Capability::Camera(Camera {
                        noise_mask_url: Some(mask.to_owned()),
                        ..Camera::default()
                    }),
                )
            })
            .component_with("front", "camera", |front| {
                front.driver(
                    Connection::Serial(Serial {
                        port: "/dev/camera".to_owned(),
                        baud: 115_200,
                    }),
                    None,
                )
            })
            .build()
            .expect("the complete simulated robot is valid");

        let plan = FullSimulationPlan::derive(&robot).expect("the plan is complete");
        let substitutions = plan.substitutions().collect::<Vec<_>>();
        assert_eq!(substitutions.len(), 1);
        assert_eq!(substitutions[0].participant().as_str(), "front");
        assert_eq!(
            substitutions[0]
                .capabilities()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["front.image"]
        );
        assert_eq!(
            plan.required_assets()
                .map(AssetId::as_str)
                .collect::<Vec<_>>(),
            [mask]
        );
        assert!(matches!(
            plan.validate_assets(|_| None),
            Err(FullSimulationError::MissingAsset { asset }) if asset.as_str() == mask
        ));
        plan.validate_assets(|asset| (asset.as_str() == mask).then_some(17))
            .expect("the bundled mask closes the simulation plan");
    }
}
