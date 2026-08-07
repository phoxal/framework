//! The rules a parsed `robot.yaml` v0 document must satisfy.
//!
//! Every check appends to one list rather than returning early: an author
//! fixing a document should see everything that is wrong with it at once.
//!
//! Identifier *shape* is not checked here for values the DTO already parses
//! into a validated canonical identifier. A `component.capability` reference,
//! for instance, cannot reach this point malformed - it is rejected while the
//! document is being deserialized.

use std::collections::BTreeSet;

use phoxal_model::identity::is_valid_token;

use super::{KinematicConfig, Manifest, Parameters, ValidationError};

impl Manifest {
    pub(super) fn validate_component_structure(&self, errors: &mut Vec<ValidationError>) {
        for (component_id, component) in &self.robot.components {
            if !is_valid_token(component_id) {
                errors.push(ValidationError::InvalidToken {
                    field: format!("robot.components.{component_id}"),
                    value: component_id.clone(),
                });
            }
            if component.component.trim().is_empty() {
                errors.push(ValidationError::EmptyComponentType {
                    instance: component_id.clone(),
                });
            }
            if !is_valid_token(&component.component) {
                errors.push(ValidationError::InvalidToken {
                    field: format!("robot.components.{component_id}.component"),
                    value: component.component.clone(),
                });
            }

            if component.mount_link.trim().is_empty() {
                errors.push(ValidationError::EmptyMountLink {
                    instance: component_id.clone(),
                });
            }

            for capability_key in component.parameters.keys() {
                if !is_valid_token(capability_key) {
                    errors.push(ValidationError::InvalidToken {
                        field: format!("robot.components.{component_id}.parameters"),
                        value: capability_key.clone(),
                    });
                }
            }

            for capability_id in component.roles.keys() {
                if !is_valid_token(capability_id) {
                    errors.push(ValidationError::InvalidToken {
                        field: format!("robot.components.{component_id}.roles"),
                        value: capability_id.clone(),
                    });
                }
            }
        }
    }

    pub(super) fn validate_driver_structure(&self, errors: &mut Vec<ValidationError>) {
        for (component_id, component) in &self.robot.components {
            let Some(driver) = &component.driver else {
                continue;
            };
            if driver.runtime_clock_ms == 0 {
                errors.push(ValidationError::InvalidRuntimeClock {
                    instance: component_id.clone(),
                });
            }
            // A component driver owns physical hardware IO; stepping it with
            // simulated time is never a meaningful robot.
            if self.clock == super::Clock::Simulated {
                errors.push(ValidationError::DriverUnderSimulatedClock {
                    instance: component_id.clone(),
                });
            }
        }
    }

    pub(super) fn validate_role_hints(&self, errors: &mut Vec<ValidationError>) {
        for (component_id, component) in &self.robot.components {
            for (capability_id, roles) in &component.roles {
                if roles.is_empty() {
                    errors.push(ValidationError::EmptyRoleList {
                        instance: component_id.clone(),
                        capability: capability_id.clone(),
                    });
                }
                let mut seen = BTreeSet::new();
                for role in roles {
                    if !seen.insert(*role) {
                        errors.push(ValidationError::RepeatedRole {
                            instance: component_id.clone(),
                            capability: capability_id.clone(),
                            role: *role,
                        });
                    }
                }
            }
        }
    }

    pub(super) fn validate_kinematics(&self, errors: &mut Vec<ValidationError>) {
        let mut positive = |value: f64, field: &str| {
            if !is_valid_positive_f64(value) {
                errors.push(ValidationError::InvalidKinematicField {
                    field: field.to_string(),
                    message: "must be > 0".to_string(),
                });
            }
        };
        match &self.robot.kinematic {
            KinematicConfig::Differential {
                left_actuators,
                right_actuators,
                left_encoders,
                right_encoders,
                wheel_radius_m,
                wheel_base_m,
            } => {
                positive(*wheel_radius_m, "wheel_radius_m");
                positive(*wheel_base_m, "wheel_base_m");
                for (references, field, capability_kind) in [
                    (left_actuators, "left_actuators", "actuator"),
                    (right_actuators, "right_actuators", "actuator"),
                    (left_encoders, "left_encoders", "encoder"),
                    (right_encoders, "right_encoders", "encoder"),
                ] {
                    require_non_empty(references, field, capability_kind, errors);
                }
            }
            KinematicConfig::Mecanum {
                wheel_radius_m,
                wheel_base_m,
                track_m,
                ..
            } => {
                positive(*wheel_radius_m, "wheel_radius_m");
                positive(*wheel_base_m, "wheel_base_m");
                positive(*track_m, "track_m");
            }
            KinematicConfig::Ackermann {
                wheel_base_m,
                track_m,
                max_steering_angle_rad,
                ..
            } => {
                positive(*wheel_base_m, "wheel_base_m");
                positive(*track_m, "track_m");
                positive(*max_steering_angle_rad, "max_steering_angle_rad");
            }
            KinematicConfig::Omnidirectional { actuators, .. } => {
                require_non_empty(actuators, "actuators", "actuator", errors);
            }
        }
    }

    pub(super) fn validate_numerics(&self, errors: &mut Vec<ValidationError>) {
        for (value, field) in [
            (
                self.robot.motion_limits.max_linear_speed_mps,
                "max_linear_speed_mps",
            ),
            (
                self.robot.motion_limits.max_angular_speed_radps,
                "max_angular_speed_radps",
            ),
        ] {
            if !is_valid_motion_limit(value) {
                errors.push(ValidationError::InvalidMotionLimit {
                    field: field.to_string(),
                    message: "must be finite and > 0".to_string(),
                });
            }
        }
        for (component_id, component) in &self.robot.components {
            for (capability_id, parameters) in &component.parameters {
                if !matches!(parameters, Parameters::Motor(_) | Parameters::Encoder(_)) {
                    continue;
                }
                if !matches!(parameters.direction_sign(), -1 | 1) {
                    errors.push(ValidationError::InvalidDirectionSign {
                        instance: component_id.clone(),
                        capability: capability_id.clone(),
                    });
                }
            }
        }
    }
}

/// A kinematic model that lists no capability of a kind it needs cannot drive
/// anything, so an empty list is a rejection rather than a degenerate robot.
fn require_non_empty<T>(
    references: &[T],
    field: &str,
    capability_kind: &str,
    errors: &mut Vec<ValidationError>,
) {
    if references.is_empty() {
        errors.push(ValidationError::InvalidKinematicField {
            field: field.to_string(),
            message: format!("must list at least one {capability_kind}"),
        });
    }
}

fn is_valid_positive_f64(value: f64) -> bool {
    value.is_finite() && value > f64::EPSILON
}

/// Motion limits cross the wire as `f32`, so a value that does not survive the
/// narrowing is not a usable limit however finite it is in `f64`.
fn is_valid_motion_limit(value: f64) -> bool {
    is_valid_positive_f64(value) && value <= f64::from(f32::MAX)
}

#[cfg(test)]
mod tests {
    use crate::source::robot::Manifest;
    use crate::source::robot::v0::ValidationError;

    /// A manifest whose single component declares role hints for two
    /// capabilities. `depth_roles` is spliced in as the `depth:` entry.
    fn manifest_with_depth_roles(depth_roles: &str) -> String {
        format!(
            r#"
schema: phoxal/robot/v0
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

    fn role_violations(depth_roles: &str) -> Vec<ValidationError> {
        let error = Manifest::parse(&manifest_with_depth_roles(depth_roles))
            .expect_err("the document must be rejected");
        error
            .violations()
            .and_then(crate::source::Violations::robot)
            .expect("a rejected robot document carries robot violations")
            .to_vec()
    }

    #[test]
    fn distinct_role_hints_for_one_capability_validate() {
        Manifest::parse(&manifest_with_depth_roles("[localization, mapping]"))
            .expect("distinct roles on one capability are legal");
    }

    #[test]
    fn an_empty_role_list_is_a_validation_error() {
        let errors = role_violations("[]");
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
        let errors = role_violations("[mapping, mapping]");
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

    /// A capability reference is a validated canonical identity, so a
    /// whitespace-padded one is refused while the document is deserialized
    /// rather than surviving into the model.
    ///
    /// Padding matters because a canonical identity is compared byte for byte
    /// downstream: if it were trimmed on the way in, `" drive.motor"` and
    /// `"drive.motor"` would name one runtime capability while reading as two
    /// authored ones.
    #[test]
    fn a_padded_capability_reference_is_refused_at_parse_time() {
        for padded in [
            " drive.motor",
            "drive. motor",
            "drive.motor ",
            "Drive.motor",
        ] {
            let document = format!(
                r#"
schema: phoxal/robot/v0
robot:
  id: test-bot
  namespace: dev
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: ["{padded}"]
    encoders: []
  components: {{}}
"#
            );
            let error = Manifest::parse(&document)
                .expect_err("a padded capability reference must be refused");
            assert!(
                error.to_string().contains("failed to parse robot document"),
                "'{padded}' must be refused while parsing, got: {error}"
            );
        }
    }
}
