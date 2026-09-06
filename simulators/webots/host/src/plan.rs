//! Host-private lowering from a compiled Robot to controller wire records.

use std::collections::BTreeMap;

use phoxal::model::Robot;
use phoxal::model::asset::AssetId;
use phoxal::model::component::capability::{
    Capability as DeclaredCapability, CapabilityKind, GnssCoordinateSystem, MotorCommand,
    StructuralTarget,
};
use phoxal::model::identity::CapabilityRef;
use phoxal::model::simulation::{
    ActuatorType, Capability as SimulatedCapability, FullSimulationError, FullSimulationPlan,
};
use phoxal_simulator_webots_shared::plan::{
    CapabilityBinding, DriverSubstitution, LinkSimulation, PlannedAsset, PlannedTarget,
    RobotSimulationPlan, SampledCapabilityKind, SamplingPlan,
};
use sha2::{Digest as _, Sha256};

const NANOS_PER_SECOND: f64 = 1_000_000_000.0;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum PlanError {
    #[error("Webots basicTimeStep must be a positive whole millisecond")]
    InvalidTimeStep,
    #[error("Webots does not support {kind} capability '{capability}': {detail}")]
    UnsupportedCapability {
        capability: String,
        kind: String,
        detail: String,
    },
    #[error(
        "motor '{capability}' declares {declared:?}, but simulation config requests {simulated:?}"
    )]
    ActuationMismatch {
        capability: String,
        declared: MotorCommand,
        simulated: ActuatorType,
    },
    #[error("capability '{capability}' has invalid cadence: {detail}")]
    InvalidCadence { capability: String, detail: String },
    #[error(transparent)]
    DriveAuthority(#[from] phoxal::drive::authority::DriveAuthorityError),
    #[error("native Webots device name '{device}' is claimed by both {first} and {second}")]
    DuplicateDevice {
        device: String,
        first: String,
        second: String,
    },
    #[error("required simulation asset '{asset}' is unavailable: {detail}")]
    MissingAsset { asset: String, detail: String },
    #[error("required simulation asset '{asset}' is empty")]
    EmptyAsset { asset: String },
    #[error(transparent)]
    FullSimulation(#[from] FullSimulationError),
}

pub(crate) fn required_assets(robot: &Robot) -> Result<FullSimulationPlan, PlanError> {
    Ok(FullSimulationPlan::derive(robot)?)
}

#[cfg(test)]
pub(crate) fn derive_robot_plan<F, E>(
    robot: &Robot,
    basic_time_step_ms: i32,
    resolve_asset: F,
) -> Result<RobotSimulationPlan, PlanError>
where
    F: FnMut(&AssetId) -> Result<Vec<u8>, E>,
    E: std::fmt::Display,
{
    let full = required_assets(robot)?;
    lower_robot_plan(robot, &full, basic_time_step_ms, resolve_asset)
}

pub(crate) fn lower_robot_plan<F, E>(
    robot: &Robot,
    full: &FullSimulationPlan,
    basic_time_step_ms: i32,
    mut resolve_asset: F,
) -> Result<RobotSimulationPlan, PlanError>
where
    F: FnMut(&AssetId) -> Result<Vec<u8>, E>,
    E: std::fmt::Display,
{
    if basic_time_step_ms <= 0 {
        return Err(PlanError::InvalidTimeStep);
    }
    let substitutions = full
        .substitutions()
        .map(|substitution| DriverSubstitution {
            participant: substitution.participant().clone(),
            capabilities: substitution.capabilities().cloned().collect(),
        })
        .collect();
    let mut capabilities = Vec::new();
    let mut links = Vec::new();
    let mut claimed_devices = BTreeMap::<String, String>::new();
    for component in robot.components() {
        let component_id = component.id();
        let declared = component.component_type();
        let simulation = component.simulation().ok_or_else(|| {
            PlanError::FullSimulation(FullSimulationError::MissingSimulation {
                component: component_id.clone(),
            })
        })?;
        for (capability_id, capability) in declared.capabilities() {
            let reference = CapabilityRef::new(component_id.clone(), capability_id.clone());
            let simulated = simulation
                .capability(capability_id.as_str())
                .ok_or_else(|| {
                    PlanError::FullSimulation(FullSimulationError::MissingCapability {
                        capability: reference.clone(),
                    })
                })?;
            let binding =
                bind_capability(reference.clone(), capability, simulated, basic_time_step_ms)?;
            if matches!(capability, DeclaredCapability::Motor(_)) {
                phoxal::drive::authority::DriveCommandAuthority::validate_motor(robot, &reference)?;
            }
            validate_target(&reference, capability, declared.structure())?;
            for device in native_device_names(&binding) {
                if let Some(first) = claimed_devices.insert(device.clone(), reference.to_string()) {
                    return Err(PlanError::DuplicateDevice {
                        device,
                        first,
                        second: reference.to_string(),
                    });
                }
            }
            capabilities.push(binding);
        }
        for (link, config) in simulation.links() {
            if declared.structure().link(link.as_str()).is_none() {
                return Err(unsupported(
                    component_id.to_string(),
                    "link_simulation",
                    format!("simulation link '{link}' is absent from the component structure"),
                ));
            }
            if config.contact_material().is_some()
                && !has_movable_parent(declared.structure(), link.as_str())
            {
                return Err(unsupported(
                    component_id.to_string(),
                    "contact_material",
                    format!(
                        "contact material on rigidly mounted link '{link}' cannot be represented independently after fixed-body aggregation"
                    ),
                ));
            }
            links.push(LinkSimulation {
                component: component_id.clone(),
                link: link.to_string(),
                contact_material: config.contact_material().map(str::to_owned),
            });
        }
    }
    let mut assets = Vec::new();
    for id in full.required_assets() {
        let bytes = resolve_asset(id).map_err(|error| PlanError::MissingAsset {
            asset: id.to_string(),
            detail: error.to_string(),
        })?;
        if bytes.is_empty() {
            return Err(PlanError::EmptyAsset {
                asset: id.to_string(),
            });
        }
        assets.push(PlannedAsset {
            id: id.clone(),
            bytes: u64::try_from(bytes.len()).map_err(|_| PlanError::MissingAsset {
                asset: id.to_string(),
                detail: "length does not fit u64".to_owned(),
            })?,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        });
    }
    Ok(RobotSimulationPlan {
        robot: robot.id().to_string(),
        basic_time_step_ms,
        substitutions,
        capabilities,
        links,
        assets,
    })
}

fn unsupported(capability: String, kind: impl Into<String>, detail: String) -> PlanError {
    PlanError::UnsupportedCapability {
        capability,
        kind: kind.into(),
        detail,
    }
}

fn validate_target(
    reference: &CapabilityRef,
    capability: &DeclaredCapability,
    structure: &phoxal::model::structure::Structure,
) -> Result<(), PlanError> {
    let kind = capability.kind();
    match capability.target() {
        StructuralTarget::Joint { id } => {
            let joint = structure.joint(id.as_str()).ok_or_else(|| {
                unsupported(
                    reference.to_string(),
                    kind.to_string(),
                    format!("target joint '{id}' is absent from component structure"),
                )
            })?;
            if !matches!(
                joint.kind(),
                phoxal::model::structure::JointKind::Revolute
                    | phoxal::model::structure::JointKind::Continuous
                    | phoxal::model::structure::JointKind::Prismatic
            ) {
                return Err(unsupported(
                    reference.to_string(),
                    kind.to_string(),
                    format!(
                        "target joint '{id}' has unsupported {:?} native kind",
                        joint.kind()
                    ),
                ));
            }
            if matches!(kind, CapabilityKind::Motor | CapabilityKind::Encoder)
                && joint.kind() == phoxal::model::structure::JointKind::Prismatic
            {
                return Err(unsupported(reference.to_string(), kind.to_string(), "the current typed motor and encoder contracts use rotational units and cannot bind a linear Webots joint".to_owned()));
            }
        }
        StructuralTarget::Link { id } if structure.link(id.as_str()).is_none() => {
            return Err(unsupported(
                reference.to_string(),
                kind.to_string(),
                format!("target link '{id}' is absent from component structure"),
            ));
        }
        StructuralTarget::Link { .. } => {}
    }
    Ok(())
}

fn has_movable_parent(structure: &phoxal::model::structure::Structure, link: &str) -> bool {
    structure
        .parent_joint(link)
        .is_some_and(|joint| joint.kind() != phoxal::model::structure::JointKind::Fixed)
}

fn bind_capability(
    reference: CapabilityRef,
    declared: &DeclaredCapability,
    simulated: &SimulatedCapability,
    step_ms: i32,
) -> Result<CapabilityBinding, PlanError> {
    let kind = declared.kind();
    let supported = matches!(
        kind,
        CapabilityKind::Motor
            | CapabilityKind::Encoder
            | CapabilityKind::Accelerometer
            | CapabilityKind::Gyroscope
            | CapabilityKind::Imu
            | CapabilityKind::Gnss
            | CapabilityKind::Camera
            | CapabilityKind::Depth
            | CapabilityKind::Range
    );
    if !supported {
        return Err(unsupported(reference.to_string(), kind.to_string(), "the R2025a adapter currently has no complete native generation and typed I/O path for this capability".to_owned()));
    }
    if let DeclaredCapability::Gnss(config) = declared
        && config.coordinate_system != GnssCoordinateSystem::Wgs84
    {
        return Err(unsupported(reference.to_string(), kind.to_string(), "the typed GNSS sample is geographic, so Webots admission requires an explicit wgs84 coordinate system".to_owned()));
    }
    if let SimulatedCapability::Motor(config) = simulated {
        if config.sampling_period_torque_hz.is_some() {
            return Err(unsupported(
                reference.to_string(),
                kind.to_string(),
                "torque-feedback sampling has no typed publication path in the v0 adapter"
                    .to_owned(),
            ));
        }
        if let Some(pid) = &config.control_pid
            && pid.len() != 3
        {
            return Err(unsupported(
                reference.to_string(),
                kind.to_string(),
                "Webots control_pid must contain exactly P, I, and D".to_owned(),
            ));
        }
    }
    let joint_device = matches!(kind, CapabilityKind::Motor | CapabilityKind::Encoder);
    if joint_device != matches!(declared.target(), StructuralTarget::Joint { .. }) {
        return Err(unsupported(
            reference.to_string(),
            kind.to_string(),
            if joint_device {
                "Webots motor and encoder devices must target a movable joint"
            } else {
                "Webots sampled body devices must target a link"
            }
            .to_owned(),
        ));
    }
    let target = match declared.target().namespaced(&reference.component_id) {
        StructuralTarget::Link { id } => PlannedTarget::Link { id: id.to_string() },
        StructuralTarget::Joint { id } => PlannedTarget::Joint { id: id.to_string() },
    };
    let native_device = reference.to_string();
    if let (DeclaredCapability::Motor(config), SimulatedCapability::Motor(native)) =
        (declared, simulated)
    {
        let expected = match config.command {
            MotorCommand::Position => ActuatorType::Position,
            MotorCommand::Velocity => ActuatorType::Velocity,
            MotorCommand::Torque => ActuatorType::Torque,
        };
        if native.actuator_type != expected {
            return Err(PlanError::ActuationMismatch {
                capability: reference.to_string(),
                declared: config.command,
                simulated: native.actuator_type,
            });
        }
        return Ok(CapabilityBinding::Motor {
            reference,
            native_device,
            target,
            command: config.command,
        });
    }
    let (publish_rate_hz, native_sampling_rate_hz) = sampling_rates(declared, simulated)
        .ok_or_else(|| {
            unsupported(
                reference.to_string(),
                kind.to_string(),
                "the compiled native I/O contract is incomplete for this capability".to_owned(),
            )
        })?;
    let sampling = sampling_plan(
        &reference,
        step_ms,
        publish_rate_hz,
        native_sampling_rate_hz,
    )?;
    if kind == CapabilityKind::Encoder {
        return Ok(CapabilityBinding::Encoder {
            reference,
            native_device,
            target,
            sampling,
        });
    }
    let capability = SampledCapabilityKind::from_capability_kind(kind).ok_or_else(|| {
        unsupported(
            reference.to_string(),
            kind.to_string(),
            "the compiled native I/O contract is incomplete for this capability".to_owned(),
        )
    })?;
    Ok(CapabilityBinding::Sampled {
        reference,
        native_device,
        target,
        capability,
        sampling,
    })
}

fn native_device_names(binding: &CapabilityBinding) -> Vec<String> {
    let mut devices = vec![binding.native_device().to_owned()];
    if binding.kind() == CapabilityKind::Imu {
        devices.extend([
            format!("{}__accel", binding.native_device()),
            format!("{}__gyro", binding.native_device()),
        ]);
    }
    devices
}

fn sampling_rates(
    declared: &DeclaredCapability,
    simulated: &SimulatedCapability,
) -> Option<(f64, f64)> {
    Some(match (declared, simulated) {
        (DeclaredCapability::Encoder(a), SimulatedCapability::Encoder(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Accelerometer(a), SimulatedCapability::Accelerometer(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Gyroscope(a), SimulatedCapability::Gyroscope(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Imu(a), SimulatedCapability::Imu(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Gnss(a), SimulatedCapability::Gnss(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Camera(a), SimulatedCapability::Camera(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Depth(a), SimulatedCapability::Depth(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        (DeclaredCapability::Range(a), SimulatedCapability::Range(b)) => {
            (a.publish_rate_hz, b.sampling_period_hz)
        }
        _ => return None,
    })
}

fn sampling_plan(
    reference: &CapabilityRef,
    step_ms: i32,
    publish_rate_hz: f64,
    native_rate_hz: f64,
) -> Result<SamplingPlan, PlanError> {
    let failure = |detail: &str| PlanError::InvalidCadence {
        capability: reference.to_string(),
        detail: detail.to_owned(),
    };
    if !publish_rate_hz.is_finite() || publish_rate_hz <= 0.0 {
        return Err(failure("publish rate must be finite and positive"));
    }
    if !native_rate_hz.is_finite() || native_rate_hz <= 0.0 {
        return Err(failure("native sampling rate must be finite and positive"));
    }
    let requested = (1000.0 / native_rate_hz).round().max(1.0);
    if requested > f64::from(i32::MAX) {
        return Err(failure("native sampling period exceeds Webots range"));
    }
    let basic = u64::try_from(step_ms).map_err(|_| failure("world step is invalid"))?;
    let requested = requested as u64;
    let periods = requested
        .checked_add(basic - 1)
        .and_then(|value| value.checked_div(basic))
        .ok_or_else(|| failure("native sampling quantization overflowed"))?;
    let native_period = periods
        .checked_mul(basic)
        .ok_or_else(|| failure("native sampling quantization overflowed"))?;
    let publish_period = (NANOS_PER_SECOND / publish_rate_hz).round();
    if !(1.0..=u64::MAX as f64).contains(&publish_period) {
        return Err(failure("publish period does not fit nanoseconds"));
    }
    Ok(SamplingPlan {
        publish_rate_hz,
        native_sampling_rate_hz: native_rate_hz,
        native_period_ms: i32::try_from(native_period)
            .map_err(|_| failure("native sampling period exceeds Webots range"))?,
        publish_period_ns: publish_period as u64,
    })
}
