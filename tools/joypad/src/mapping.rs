use phoxal::api::motion::v1::ManualCommand;
use phoxal::api::safety::v1::EmergencyStopRequest;
use phoxal::model::robot::v1::KinematicConfig;

/// Full-scale teleop targets; the motion runtime re-clamps these to the live safety envelope.
pub const MANUAL_MAX_LINEAR_MPS: f64 = 0.6;
pub const MANUAL_MAX_ANGULAR_RADPS: f64 = 2.0;
const DEADZONE: f32 = 0.1;

/// gilrs normalizes sticks to [-1.0, 1.0] with up and right positive; triggers to [0.0, 1.0].
#[derive(Debug, Clone, Copy, Default)]
pub struct GamepadState {
    #[allow(dead_code)]
    pub left_stick_x: f32,
    pub left_stick_y: f32,
    pub right_stick_x: f32,
    pub left_trigger: f32,
    pub right_trigger: f32,
    pub l1: bool,
    pub r1: bool,
    pub emergency_stop: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlScheme {
    /// Tank drive (differential robots): L2/R2 triggers drive the left/right wheels forward,
    /// L1/R1 bumpers drive them backward.
    DifferentialTank,
    /// Left stick = forward/back, right stick X = yaw (non-differential robots).
    Stick,
}

impl ControlScheme {
    pub fn from_kinematic(kinematic: &KinematicConfig) -> Self {
        if matches!(kinematic, KinematicConfig::Differential { .. }) {
            Self::DifferentialTank
        } else {
            Self::Stick
        }
    }
}

pub fn map_command(scheme: ControlScheme, state: &GamepadState) -> ManualCommand {
    match scheme {
        ControlScheme::DifferentialTank => differential_command(state),
        ControlScheme::Stick => stick_command(state),
    }
}

pub fn map_emergency_stop_request(state: &GamepadState) -> EmergencyStopRequest {
    EmergencyStopRequest {
        engaged: state.emergency_stop,
    }
}

/// Tank drive: each side's forward magnitude comes from its analog trigger (L2/R2), reversed by its
/// bumper (L1/R1). The per-side values combine into forward (average) and yaw (right minus left).
fn differential_command(state: &GamepadState) -> ManualCommand {
    let left = signed_trigger(state.left_trigger, state.l1);
    let right = signed_trigger(state.right_trigger, state.r1);
    let forward = ((left + right) * 0.5).clamp(-1.0, 1.0);
    // Right side faster than left -> turn left (CCW, +angular_z per REP-103).
    let yaw = ((right - left) * 0.5).clamp(-1.0, 1.0);
    ManualCommand {
        linear_x_mps: f64::from(forward) * MANUAL_MAX_LINEAR_MPS,
        angular_z_radps: f64::from(yaw) * MANUAL_MAX_ANGULAR_RADPS,
    }
}

fn stick_command(state: &GamepadState) -> ManualCommand {
    let forward = deadzoned(state.left_stick_y);
    let yaw = deadzoned(state.right_stick_x);
    ManualCommand {
        linear_x_mps: f64::from(forward) * MANUAL_MAX_LINEAR_MPS,
        angular_z_radps: -f64::from(yaw) * MANUAL_MAX_ANGULAR_RADPS,
    }
}

fn signed_trigger(value: f32, reverse: bool) -> f32 {
    let magnitude = trigger_value(value);
    if reverse { -magnitude } else { magnitude }
}

fn trigger_value(value: f32) -> f32 {
    let normalized = value.clamp(0.0, 1.0);
    if normalized < DEADZONE {
        0.0
    } else {
        ((normalized - DEADZONE) / (1.0 - DEADZONE)).clamp(0.0, 1.0)
    }
}

fn deadzoned(value: f32) -> f32 {
    if value.abs() < DEADZONE {
        0.0
    } else {
        value.clamp(-1.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn differential_both_triggers_drive_straight_forward() {
        let cmd = map_command(
            ControlScheme::DifferentialTank,
            &GamepadState {
                left_trigger: 1.0,
                right_trigger: 1.0,
                ..Default::default()
            },
        );
        assert_eq!(cmd.linear_x_mps, MANUAL_MAX_LINEAR_MPS);
        assert_eq!(cmd.angular_z_radps, 0.0);
    }

    #[test]
    fn differential_right_trigger_only_turns_left() {
        let cmd = map_command(
            ControlScheme::DifferentialTank,
            &GamepadState {
                right_trigger: 1.0,
                ..Default::default()
            },
        );
        assert!(cmd.angular_z_radps > 0.0);
        assert!(cmd.linear_x_mps > 0.0);
    }

    #[test]
    fn differential_bumpers_reverse_each_side() {
        let cmd = map_command(
            ControlScheme::DifferentialTank,
            &GamepadState {
                left_trigger: 1.0,
                right_trigger: 1.0,
                l1: true,
                r1: true,
                ..Default::default()
            },
        );
        assert_eq!(cmd.linear_x_mps, -MANUAL_MAX_LINEAR_MPS);
        assert_eq!(cmd.angular_z_radps, 0.0);
    }

    #[test]
    fn differential_counter_rotate_spins_in_place() {
        let cmd = map_command(
            ControlScheme::DifferentialTank,
            &GamepadState {
                left_trigger: 1.0,
                l1: true,
                right_trigger: 1.0,
                ..Default::default()
            },
        );
        assert_eq!(cmd.linear_x_mps, 0.0);
        assert!(cmd.angular_z_radps > 0.0);
    }

    #[test]
    fn stick_scheme_uses_left_stick_forward_and_right_stick_yaw() {
        let cmd = map_command(
            ControlScheme::Stick,
            &GamepadState {
                left_stick_y: 1.0,
                right_stick_x: 1.0,
                ..Default::default()
            },
        );
        assert_eq!(cmd.linear_x_mps, MANUAL_MAX_LINEAR_MPS);
        assert_eq!(cmd.angular_z_radps, -MANUAL_MAX_ANGULAR_RADPS);
    }

    #[test]
    fn scheme_from_kinematic() {
        assert_eq!(
            ControlScheme::from_kinematic(&KinematicConfig::Differential {
                left_actuators: Vec::new(),
                right_actuators: Vec::new(),
                left_encoders: Vec::new(),
                right_encoders: Vec::new(),
                wheel_radius_m: 0.1,
                wheel_base_m: 0.4,
            }),
            ControlScheme::DifferentialTank
        );
        assert_eq!(
            ControlScheme::from_kinematic(&KinematicConfig::Omnidirectional {
                actuators: Vec::new(),
                encoders: Vec::new(),
            }),
            ControlScheme::Stick
        );
    }

    #[test]
    fn emergency_stop_button_maps_to_engaged_request() {
        let request = map_emergency_stop_request(&GamepadState {
            emergency_stop: true,
            ..Default::default()
        });

        assert_eq!(request, EmergencyStopRequest { engaged: true });
    }
}
