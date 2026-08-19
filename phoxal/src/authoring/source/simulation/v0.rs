//! Exact `simulation.yaml` v0 document.
//!
//! One component type's simulated behaviour: the per-capability parameters a
//! simulator needs, and the contact material of each component-local link.

use std::collections::BTreeMap;
use std::fmt;

use crate::model::identity::is_valid_token;
use serde::{Deserialize, Serialize};

// The simulator's closed vocabularies are canonical, so this layer describes
// the one definition rather than keeping a second copy of it. `crate::model`
// stays their only path.
use crate::model::component::capability::CapabilityKind;
use crate::model::simulation::{ActuatorType, CameraProjection};

/// Exact top-level `simulation.yaml` v0 document.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default)]
    pub capabilities: BTreeMap<String, Capability>,
    #[serde(default)]
    pub links: BTreeMap<String, Link>,
}

/// The simulated properties of one component-local link.
#[derive(Debug, Clone, Serialize, Deserialize, Default, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Link {
    #[serde(default)]
    pub contact_material: Option<String>,
}

/// One authored rule a `simulation.yaml` v0 document broke.
///
/// `field` is the authored path to the offending value, so a rejection points
/// at the line an author has to edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// A capability id is not a normalized token.
    InvalidCapabilityId { capability: String },
    /// A link is keyed by a blank name.
    EmptyLinkName,
    /// A link declares a contact material with no name.
    EmptyContactMaterial { link: String },
    /// A numeric field is NaN or infinite.
    NotFinite { field: String },
    /// A numeric field that scales a physical quantity is not positive.
    NotPositive { field: String },
    /// A PID triple does not carry three terms.
    PidTermCount { field: String, found: usize },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCapabilityId { capability } => write!(
                formatter,
                "simulation.capabilities.{capability} must use a valid capability token"
            ),
            Self::EmptyLinkName => {
                formatter.write_str("simulation.links contains an empty link name")
            }
            Self::EmptyContactMaterial { link } => write!(
                formatter,
                "simulation.links.{link}.contact_material must not be empty"
            ),
            Self::NotFinite { field } => write!(formatter, "{field} must be finite"),
            Self::NotPositive { field } => write!(formatter, "{field} must be finite and > 0"),
            Self::PidTermCount { field, found } => write!(
                formatter,
                "{field} must contain exactly 3 terms, found {found}"
            ),
        }
    }
}

impl Manifest {
    /// Resolve this generation's grammar into the version-independent
    /// simulation.
    ///
    /// The authored capability map and its canonical counterpart are the same
    /// wire shape by construction, so this generation adopts it wholesale; a
    /// link's authored name becomes the canonical link identity it always
    /// referred to.
    ///
    /// # Errors
    ///
    /// Returns [`crate::authoring::CompileError::Transcode`] when an authored capability
    /// does not adopt into its canonical counterpart.
    pub(crate) fn normalize(
        self,
    ) -> Result<crate::authoring::normalized::Simulation, crate::authoring::CompileError> {
        Ok(crate::authoring::normalized::Simulation {
            capabilities: crate::authoring::source::transcode(
                &self.capabilities,
                "simulation capabilities",
            )?,
            links: self
                .links
                .into_iter()
                .map(|(id, link)| {
                    (
                        crate::model::identity::LinkId::new(id),
                        link.contact_material,
                    )
                })
                .collect(),
        })
    }

    /// Every rule this document breaks, or `Ok(())` when it breaks none.
    ///
    /// # Errors
    ///
    /// Returns every [`ValidationError`] at once.
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();

        for (capability_id, capability) in &self.capabilities {
            if !is_valid_token(capability_id) {
                errors.push(ValidationError::InvalidCapabilityId {
                    capability: capability_id.clone(),
                });
            }
            capability.validate(
                &format!("simulation.capabilities.{capability_id}"),
                &mut errors,
            );
        }

        for (link_name, link) in &self.links {
            if link_name.trim().is_empty() {
                errors.push(ValidationError::EmptyLinkName);
            }
            if let Some(contact_material) = &link.contact_material
                && contact_material.trim().is_empty()
            {
                errors.push(ValidationError::EmptyContactMaterial {
                    link: link_name.clone(),
                });
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// The simulator parameters for one capability: the noise, resolutions and
/// sampling a world model needs on top of the physical (URDF) and component
/// facts, which it never restates.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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

    fn validate(&self, field: &str, errors: &mut Vec<ValidationError>) {
        match self {
            Self::Motor(config) => {
                validate_optional_finite(
                    config.acceleration_radps2,
                    field,
                    "acceleration_radps2",
                    errors,
                );
                validate_optional_positive(
                    config.sampling_period_torque_hz,
                    field,
                    "sampling_period_torque_hz",
                    errors,
                );
                if let Some(pid) = &config.control_pid {
                    if pid.len() != 3 {
                        errors.push(ValidationError::PidTermCount {
                            field: format!("{field}.control_pid"),
                            found: pid.len(),
                        });
                    }
                    if pid.iter().any(|value| !value.is_finite()) {
                        errors.push(ValidationError::NotFinite {
                            field: format!("{field}.control_pid"),
                        });
                    }
                }
            }
            Self::Encoder(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_optional_finite(config.noise, field, "noise", errors);
            }
            Self::Accelerometer(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_table(
                    config.lookup_table.as_deref(),
                    field,
                    "lookup_table",
                    errors,
                );
            }
            Self::Gyroscope(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_table(
                    config.lookup_table.as_deref(),
                    field,
                    "lookup_table",
                    errors,
                );
            }
            Self::Magnetometer(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_table(
                    config.lookup_table.as_deref(),
                    field,
                    "lookup_table",
                    errors,
                );
            }
            Self::Imu(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_optional_finite(config.noise, field, "noise", errors);
            }
            Self::Gnss(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_optional_finite(config.accuracy, field, "accuracy", errors);
                validate_optional_finite(
                    config.noise_correlation,
                    field,
                    "noise_correlation",
                    errors,
                );
                validate_optional_finite(
                    config.speed_resolution,
                    field,
                    "speed_resolution",
                    errors,
                );
                validate_optional_finite(config.speed_noise, field, "speed_noise", errors);
            }
            Self::Camera(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.near, field, "near", errors);
                validate_optional_finite(config.far, field, "far", errors);
                validate_optional_finite(config.exposure, field, "exposure", errors);
                validate_optional_finite(
                    config.ambient_occlusion_radius,
                    field,
                    "ambient_occlusion_radius",
                    errors,
                );
                validate_optional_finite(config.bloom_threshold, field, "bloom_threshold", errors);
                validate_optional_finite(config.noise, field, "noise", errors);
                validate_optional_finite(config.motion_blur, field, "motion_blur", errors);
            }
            Self::Depth(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.noise, field, "noise", errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_optional_finite(config.motion_blur, field, "motion_blur", errors);
            }
            Self::Range(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.noise, field, "noise", errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
            }
            Self::Lidar(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.noise, field, "noise", errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
            }
            Self::Mmwave(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.noise, field, "noise", errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_table(
                    config.lookup_table.as_deref(),
                    field,
                    "lookup_table",
                    errors,
                );
            }
            Self::Microphone(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.aperture, field, "aperture", errors);
            }
            Self::Speaker | Self::Battery | Self::Led | Self::EmergencyStop => {}
        }
    }
}

fn validate_sampling(value: f64, field: &str, errors: &mut Vec<ValidationError>) {
    if !value.is_finite() || value <= f64::EPSILON {
        errors.push(ValidationError::NotPositive {
            field: format!("{field}.sampling_period_hz"),
        });
    }
}

fn validate_optional_finite(
    value: Option<f64>,
    field: &str,
    name: &str,
    errors: &mut Vec<ValidationError>,
) {
    if let Some(value) = value
        && !value.is_finite()
    {
        errors.push(ValidationError::NotFinite {
            field: format!("{field}.{name}"),
        });
    }
}

fn validate_optional_positive(
    value: Option<f64>,
    field: &str,
    name: &str,
    errors: &mut Vec<ValidationError>,
) {
    if let Some(value) = value
        && (!value.is_finite() || value <= f64::EPSILON)
    {
        errors.push(ValidationError::NotPositive {
            field: format!("{field}.{name}"),
        });
    }
}

fn validate_table(
    table: Option<&[Vec<f64>]>,
    field: &str,
    name: &str,
    errors: &mut Vec<ValidationError>,
) {
    let Some(table) = table else {
        return;
    };
    for row in table {
        if row.iter().any(|value| !value.is_finite()) {
            errors.push(ValidationError::NotFinite {
                field: format!("{field}.{name}"),
            });
            return;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Motor {
    #[serde(default)]
    pub actuator_type: ActuatorType,
    #[serde(default)]
    pub acceleration_radps2: Option<f64>,
    #[serde(default)]
    pub control_pid: Option<Vec<f64>>,
    #[serde(default)]
    pub sampling_period_torque_hz: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Encoder {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub noise: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Accelerometer {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Gyroscope {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Magnetometer {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Imu {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub noise: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Gnss {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub accuracy: Option<f64>,
    #[serde(default)]
    pub noise_correlation: Option<f64>,
    #[serde(default)]
    pub speed_resolution: Option<f64>,
    #[serde(default)]
    pub speed_noise: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Camera {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub projection: Option<CameraProjection>,
    #[serde(default)]
    pub near: Option<f64>,
    #[serde(default)]
    pub far: Option<f64>,
    #[serde(default)]
    pub exposure: Option<f64>,
    #[serde(default)]
    pub anti_aliasing: Option<bool>,
    #[serde(default)]
    pub ambient_occlusion_radius: Option<f64>,
    #[serde(default)]
    pub bloom_threshold: Option<f64>,
    #[serde(default)]
    pub noise: Option<f64>,
    #[serde(default)]
    pub motion_blur: Option<f64>,
    #[serde(default)]
    pub noise_mask_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Depth {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub noise: Option<f64>,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub motion_blur: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Range {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub noise: Option<f64>,
    #[serde(default)]
    pub resolution: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Lidar {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub noise: Option<f64>,
    #[serde(default)]
    pub resolution: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Mmwave {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub noise: Option<f64>,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Microphone {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub aperture: Option<f64>,
}
