use std::collections::BTreeSet;

use crate::model::component::v1::CapabilityRef;

use super::{KinematicConfig, Robot, ValidationError, capability};

impl Robot {
    pub(crate) fn validate_component_structure(
        &self,
        validation_errors: &mut Vec<ValidationError>,
    ) {
        for (component_id, component) in &self.robot.components {
            if !crate::model::component::v1::is_valid_token(component_id) {
                validation_errors.push(ValidationError::InvalidToken {
                    field: format!("robot.components.{component_id}"),
                    value: component_id.clone(),
                });
            }
            if component.component.trim().is_empty() {
                validation_errors.push(ValidationError::EmptyComponentType {
                    instance: component_id.clone(),
                });
            }
            if !crate::model::component::v1::is_valid_token(&component.component) {
                validation_errors.push(ValidationError::InvalidToken {
                    field: format!("robot.components.{component_id}.component"),
                    value: component.component.clone(),
                });
            }

            if component.mount_link.trim().is_empty() {
                validation_errors.push(ValidationError::EmptyMountLink {
                    instance: component_id.clone(),
                });
            }

            for (capability_key, parameters) in &component.parameters {
                if !crate::model::component::v1::is_valid_token(capability_key) {
                    validation_errors.push(ValidationError::InvalidToken {
                        field: format!("robot.components.{component_id}.parameters"),
                        value: capability_key.clone(),
                    });
                }
                if parameters.kind_name().trim().is_empty() {
                    validation_errors.push(ValidationError::InvalidToken {
                        field: format!(
                            "robot.components.{component_id}.parameters.{capability_key}.kind"
                        ),
                        value: String::new(),
                    });
                }
            }

            for capability_id in component.roles.keys() {
                if !crate::model::component::v1::is_valid_token(capability_id) {
                    validation_errors.push(ValidationError::InvalidToken {
                        field: format!("robot.components.{component_id}.roles"),
                        value: capability_id.clone(),
                    });
                }
            }
        }
    }

    pub(crate) fn validate_driver_structure(&self, validation_errors: &mut Vec<ValidationError>) {
        for (component_id, component) in &self.robot.components {
            if let Some(driver) = &component.driver
                && driver.runtime_clock_ms == 0
            {
                validation_errors.push(ValidationError::InvalidRuntimeClock {
                    instance: component_id.clone(),
                });
            }
        }
    }

    pub(crate) fn validate_role_hints(&self, validation_errors: &mut Vec<ValidationError>) {
        for (component_id, component) in &self.robot.components {
            for (capability_id, roles) in &component.roles {
                if roles.is_empty() {
                    validation_errors.push(ValidationError::EmptyRoleList {
                        instance: component_id.clone(),
                        capability: capability_id.clone(),
                    });
                }
                let mut seen = BTreeSet::new();
                for role in roles {
                    if !seen.insert(*role) {
                        validation_errors.push(ValidationError::RepeatedRole {
                            instance: component_id.clone(),
                            capability: capability_id.clone(),
                            role: *role,
                        });
                    }
                }
            }
        }
    }

    pub(crate) fn validate_kinematics(&self, validation_errors: &mut Vec<ValidationError>) {
        match &self.robot.kinematic {
            KinematicConfig::Differential {
                left_actuators,
                right_actuators,
                left_encoders,
                right_encoders,
                wheel_radius_m,
                wheel_base_m,
            } => {
                validate_capability_ref_list(
                    left_actuators,
                    "left_actuators",
                    "actuator",
                    validation_errors,
                );
                validate_capability_ref_list(
                    right_actuators,
                    "right_actuators",
                    "actuator",
                    validation_errors,
                );
                validate_capability_ref_list(
                    left_encoders,
                    "left_encoders",
                    "encoder",
                    validation_errors,
                );
                validate_capability_ref_list(
                    right_encoders,
                    "right_encoders",
                    "encoder",
                    validation_errors,
                );
                if !is_valid_positive_f64(*wheel_radius_m) {
                    validation_errors.push(invalid_kinematic("wheel_radius_m", "must be > 0"));
                }
                if !is_valid_positive_f64(*wheel_base_m) {
                    validation_errors.push(invalid_kinematic("wheel_base_m", "must be > 0"));
                }
            }
            KinematicConfig::Mecanum {
                front_left_actuator,
                front_right_actuator,
                rear_left_actuator,
                rear_right_actuator,
                wheel_radius_m,
                wheel_base_m,
                track_m,
            } => {
                validate_capability_ref(
                    front_left_actuator,
                    "front_left_actuator",
                    validation_errors,
                );
                validate_capability_ref(
                    front_right_actuator,
                    "front_right_actuator",
                    validation_errors,
                );
                validate_capability_ref(
                    rear_left_actuator,
                    "rear_left_actuator",
                    validation_errors,
                );
                validate_capability_ref(
                    rear_right_actuator,
                    "rear_right_actuator",
                    validation_errors,
                );
                if !is_valid_positive_f64(*wheel_radius_m) {
                    validation_errors.push(invalid_kinematic("wheel_radius_m", "must be > 0"));
                }
                if !is_valid_positive_f64(*wheel_base_m) {
                    validation_errors.push(invalid_kinematic("wheel_base_m", "must be > 0"));
                }
                if !is_valid_positive_f64(*track_m) {
                    validation_errors.push(invalid_kinematic("track_m", "must be > 0"));
                }
            }
            KinematicConfig::Ackermann {
                steering_actuator,
                drive_actuator,
                steering_encoder,
                drive_encoder,
                wheel_base_m,
                track_m,
                max_steering_angle_rad,
            } => {
                validate_capability_ref(steering_actuator, "steering_actuator", validation_errors);
                validate_capability_ref(drive_actuator, "drive_actuator", validation_errors);
                if let Some(capability_ref) = steering_encoder {
                    validate_capability_ref(capability_ref, "steering_encoder", validation_errors);
                }
                if let Some(capability_ref) = drive_encoder {
                    validate_capability_ref(capability_ref, "drive_encoder", validation_errors);
                }
                if !is_valid_positive_f64(*wheel_base_m) {
                    validation_errors.push(invalid_kinematic("wheel_base_m", "must be > 0"));
                }
                if !is_valid_positive_f64(*track_m) {
                    validation_errors.push(invalid_kinematic("track_m", "must be > 0"));
                }
                if !is_valid_positive_f64(*max_steering_angle_rad) {
                    validation_errors
                        .push(invalid_kinematic("max_steering_angle_rad", "must be > 0"));
                }
            }
            KinematicConfig::Omnidirectional {
                actuators,
                encoders,
            } => {
                if actuators.is_empty() {
                    validation_errors.push(invalid_kinematic("actuators", "must not be empty"));
                }
                for actuator in actuators {
                    validate_capability_ref(actuator, "actuator", validation_errors);
                }
                for encoder in encoders {
                    validate_capability_ref(encoder, "encoder", validation_errors);
                }
            }
        }
    }

    pub(crate) fn validate_numerics(&self, validation_errors: &mut Vec<ValidationError>) {
        for (component_id, component) in &self.robot.components {
            for (capability_id, parameters) in &component.parameters {
                match parameters {
                    capability::Parameters::Motor(motor)
                        if motor.direction_sign != -1 && motor.direction_sign != 1 =>
                    {
                        validation_errors.push(ValidationError::InvalidDirectionSign {
                            instance: component_id.clone(),
                            capability: capability_id.clone(),
                        });
                    }
                    capability::Parameters::Encoder(sensor)
                        if sensor.direction_sign != -1 && sensor.direction_sign != 1 =>
                    {
                        validation_errors.push(ValidationError::InvalidDirectionSign {
                            instance: component_id.clone(),
                            capability: capability_id.clone(),
                        });
                    }
                    _ => {}
                }
            }
        }
    }
}

fn validate_capability_ref(
    capability_ref: &CapabilityRef,
    field: &str,
    validation_errors: &mut Vec<ValidationError>,
) {
    if !crate::model::component::v1::is_valid_token(&capability_ref.component_id)
        || !crate::model::component::v1::is_valid_token(&capability_ref.capability_id)
    {
        validation_errors.push(invalid_kinematic(
            field,
            &format!("'{capability_ref}' must use valid capability tokens"),
        ));
    }
}

fn validate_capability_ref_list(
    capability_refs: &[CapabilityRef],
    field: &str,
    capability_kind: &str,
    validation_errors: &mut Vec<ValidationError>,
) {
    if capability_refs.is_empty() {
        validation_errors.push(invalid_kinematic(
            field,
            &format!("must list at least one {capability_kind}"),
        ));
    }
    for (index, capability_ref) in capability_refs.iter().enumerate() {
        validate_capability_ref(
            capability_ref,
            &format!("{field}[{index}]"),
            validation_errors,
        );
    }
}

fn is_valid_positive_f64(value: f64) -> bool {
    value.is_finite() && value > f64::EPSILON
}

fn invalid_kinematic(field: &str, message: &str) -> ValidationError {
    ValidationError::InvalidKinematicField {
        field: field.to_string(),
        message: message.to_string(),
    }
}
