use std::collections::BTreeSet;

use super::motion::{CapabilityRef, KinematicConfig};

use super::{Manifest, ValidationError, capability};

impl Manifest {
    pub(crate) fn validate_component_structure(
        &self,
        validation_errors: &mut Vec<ValidationError>,
    ) {
        for (component_id, component) in &self.robot.components {
            if !crate::source::is_valid_token(component_id) {
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
            if !crate::source::is_valid_token(&component.component) {
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
                if !crate::source::is_valid_token(capability_key) {
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
                if !crate::source::is_valid_token(capability_id) {
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
        if !is_valid_motion_limit(self.robot.motion_limits.max_linear_speed_mps) {
            validation_errors.push(ValidationError::InvalidMotionLimit {
                field: "max_linear_speed_mps".to_string(),
                message: "must be finite and > 0".to_string(),
            });
        }
        if !is_valid_motion_limit(self.robot.motion_limits.max_angular_speed_radps) {
            validation_errors.push(ValidationError::InvalidMotionLimit {
                field: "max_angular_speed_radps".to_string(),
                message: "must be finite and > 0".to_string(),
            });
        }
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
    if !crate::source::is_valid_token(&capability_ref.component_id)
        || !crate::source::is_valid_token(&capability_ref.capability_id)
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

fn is_valid_motion_limit(value: f64) -> bool {
    is_valid_positive_f64(value) && value <= f64::from(f32::MAX)
}

fn invalid_kinematic(field: &str, message: &str) -> ValidationError {
    ValidationError::InvalidKinematicField {
        field: field.to_string(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::ValidationError;

    /// A manifest whose single component declares role hints for two
    /// capabilities. `extra_roles` is spliced in as further `depth:` entries.
    fn manifest_with_depth_roles(depth_roles: &str) -> String {
        format!(
            r#"
schema: robot/v0
robot:
  id: test-bot
  namespace: dev
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: [drive.motor]
    encoders: []
  components:
    front_camera:
      component: oak_d_lite
      mount_link: front_camera_mount
      driver:
        connection: {{ type: usb }}
      roles:
        rgb: [localization]
        depth: {depth_roles}
"#
        )
    }

    #[test]
    fn distinct_role_hints_for_one_capability_validate() {
        let manifest = crate::source::robot::parse_from_string(&manifest_with_depth_roles(
            "[localization, mapping]",
        ))
        .unwrap();
        let crate::source::robot::Manifest::V0(robot) = manifest;
        robot
            .validate()
            .expect("distinct roles on one capability are legal");
    }

    #[test]
    fn an_empty_role_list_is_a_validation_error() {
        let manifest =
            crate::source::robot::parse_from_string(&manifest_with_depth_roles("[]")).unwrap();
        let crate::source::robot::Manifest::V0(robot) = manifest;
        let errors = robot.validate().expect_err("an empty role list is invalid");
        assert!(
            errors.iter().any(|error| matches!(
                error,
                ValidationError::EmptyRoleList { instance, capability }
                    if instance == "front_camera" && capability == "depth"
            )),
            "{errors:?}"
        );
    }

    #[test]
    fn a_repeated_role_names_the_capability_and_the_role() {
        let manifest = crate::source::robot::parse_from_string(&manifest_with_depth_roles(
            "[mapping, mapping]",
        ))
        .unwrap();
        let crate::source::robot::Manifest::V0(robot) = manifest;
        let errors = robot.validate().expect_err("a repeated role is invalid");
        assert!(
            errors.iter().any(|error| matches!(
                error,
                ValidationError::RepeatedRole { instance, capability, role }
                    if instance == "front_camera"
                        && capability == "depth"
                        && role.as_str() == "mapping"
            )),
            "{errors:?}"
        );
    }
}
