//! Canonical motion limits and drive kinematics.

use crate::model::error::{KinematicScalarField, ModelError, MotionLimitField};
use crate::model::identity::CapabilityRef;
use std::fmt;

/// Canonical motion facts.
#[derive(Debug, Clone)]
pub struct MotionModel {
    kinematic: KinematicConfig,
    limits: MotionLimits,
}

/// The outer envelope every motion command is clamped to.
#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct MotionLimits {
    pub max_linear_speed_mps: f64,
    pub max_angular_speed_radps: f64,
}

/// The drive geometry, and the capabilities that realize it.
#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    PartialEq,
    schemars::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum KinematicConfig {
    Differential {
        left_actuators: Vec<CapabilityRef>,
        right_actuators: Vec<CapabilityRef>,
        left_encoders: Vec<CapabilityRef>,
        right_encoders: Vec<CapabilityRef>,
        wheel_radius_m: f64,
        wheel_base_m: f64,
    },
    Mecanum {
        front_left_actuator: CapabilityRef,
        front_right_actuator: CapabilityRef,
        rear_left_actuator: CapabilityRef,
        rear_right_actuator: CapabilityRef,
        wheel_radius_m: f64,
        wheel_base_m: f64,
        track_m: f64,
    },
    Ackermann {
        steering_actuator: CapabilityRef,
        drive_actuator: CapabilityRef,
        steering_encoder: Option<CapabilityRef>,
        drive_encoder: Option<CapabilityRef>,
        wheel_base_m: f64,
        track_m: f64,
        max_steering_angle_rad: f64,
    },
    Omnidirectional {
        actuators: Vec<CapabilityRef>,
        encoders: Vec<CapabilityRef>,
    },
}

impl KinematicConfig {
    /// Number of times an actuator occurs in this compiled drive topology.
    #[must_use]
    pub fn actuator_occurrences(&self, capability: &CapabilityRef) -> usize {
        match self {
            Self::Differential {
                left_actuators,
                right_actuators,
                ..
            } => left_actuators
                .iter()
                .chain(right_actuators)
                .filter(|candidate| *candidate == capability)
                .count(),
            Self::Mecanum {
                front_left_actuator,
                front_right_actuator,
                rear_left_actuator,
                rear_right_actuator,
                ..
            } => [
                front_left_actuator,
                front_right_actuator,
                rear_left_actuator,
                rear_right_actuator,
            ]
            .into_iter()
            .filter(|candidate| *candidate == capability)
            .count(),
            Self::Ackermann {
                steering_actuator,
                drive_actuator,
                ..
            } => [steering_actuator, drive_actuator]
                .into_iter()
                .filter(|candidate| *candidate == capability)
                .count(),
            Self::Omnidirectional { actuators, .. } => actuators
                .iter()
                .filter(|candidate| *candidate == capability)
                .count(),
        }
    }

    /// The drive geometry this config describes, with its scalars validated.
    ///
    /// This is the one place the authored kinematic fields are turned into
    /// geometry, so every consumer that derives motion from the robot works from
    /// the same reading of the document.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::KinematicScalar`] when a declared scalar is not
    /// finite and positive.
    pub fn drive_kinematics(&self) -> Result<DriveKinematics, ModelError> {
        Ok(match self {
            Self::Differential {
                wheel_radius_m,
                wheel_base_m,
                ..
            } => DriveKinematics::Differential(
                DifferentialDrive::new(*wheel_radius_m, *wheel_base_m).validate()?,
            ),
            Self::Mecanum {
                wheel_radius_m,
                wheel_base_m,
                track_m,
                ..
            } => DriveKinematics::Mecanum(
                MecanumDrive::new(*wheel_radius_m, *wheel_base_m, *track_m).validate()?,
            ),
            Self::Ackermann {
                wheel_base_m,
                track_m,
                max_steering_angle_rad,
                ..
            } => DriveKinematics::Ackermann(
                AckermannDrive::new(*wheel_base_m, *track_m, *max_steering_angle_rad).validate()?,
            ),
            Self::Omnidirectional { .. } => DriveKinematics::Omnidirectional,
        })
    }
}

/// A planar body twist in the robot's base frame.
///
/// `linear_y_mps` is only meaningful for a holonomic geometry. A differential or
/// Ackermann robot cannot translate sideways at all, so those geometries ignore
/// it rather than approximating it: silently turning a commanded sideways
/// velocity into yaw would move the robot somewhere its caller did not ask for.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BodyTwist {
    /// Forward velocity, in metres per second.
    pub linear_x_mps: f64,
    /// Leftward velocity, in metres per second. Zero for a non-holonomic drive.
    pub linear_y_mps: f64,
    /// Yaw rate, in radians per second, positive counter-clockwise.
    pub angular_z_radps: f64,
}

impl BodyTwist {
    /// A twist a non-holonomic drive can realize: forward and yaw only.
    #[must_use]
    pub const fn planar(linear_x_mps: f64, angular_z_radps: f64) -> Self {
        Self {
            linear_x_mps,
            linear_y_mps: 0.0,
            angular_z_radps,
        }
    }

    /// A full holonomic twist.
    #[must_use]
    pub const fn new(linear_x_mps: f64, linear_y_mps: f64, angular_z_radps: f64) -> Self {
        Self {
            linear_x_mps,
            linear_y_mps,
            angular_z_radps,
        }
    }

    /// Whether every component is finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.linear_x_mps.is_finite()
            && self.linear_y_mps.is_finite()
            && self.angular_z_radps.is_finite()
    }
}

/// The wheel angular speeds of a differential drive, in radians per second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DifferentialWheelSpeeds {
    pub left_radps: f64,
    pub right_radps: f64,
}

/// The wheel angular speeds of a mecanum drive, in radians per second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MecanumWheelSpeeds {
    pub front_left_radps: f64,
    pub front_right_radps: f64,
    pub rear_left_radps: f64,
    pub rear_right_radps: f64,
}

/// What an Ackermann drive is commanded with.
///
/// This is a linear speed rather than a wheel angular speed because
/// [`KinematicConfig::Ackermann`] authors no wheel radius: the document has no
/// value that could convert one to the other, and inventing one here would put a
/// number on the wire that nobody authored.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AckermannCommand {
    /// Speed of the driven axle, in metres per second.
    pub drive_speed_mps: f64,
    /// Steering angle, in radians, positive counter-clockwise.
    pub steering_angle_rad: f64,
}

/// The drive geometry of a robot, in the form its kinematics need.
///
/// This is the single dispatch point over every geometry [`KinematicConfig`]
/// can declare. Each variant carries its own geometry type with its own
/// statically typed wheel commands, because the four do not share a command
/// shape: a differential drive is commanded with two wheel speeds, a mecanum
/// with four, and an Ackermann with a speed and a steering angle. Collapsing
/// them behind one signature would mean either an erased command vector or a
/// lowest-common-denominator twist, and both lose exactly the information the
/// caller needs.
///
/// Obtained from [`KinematicConfig::drive_kinematics`], which validates the
/// scalars first, so a value of this type always has usable geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DriveKinematics {
    Differential(DifferentialDrive),
    Mecanum(MecanumDrive),
    Ackermann(AckermannDrive),
    /// An omnidirectional drive, whose kinematics are not derivable from the
    /// authored document.
    ///
    /// [`KinematicConfig::Omnidirectional`] carries actuator and encoder lists
    /// and no geometry at all - no wheel radius, no wheel mounting angles, no
    /// distance from the rotation centre - and every one of those is required to
    /// relate wheel speeds to a body twist. The variant is carried here so the
    /// enum covers every geometry the model can declare, and so a consumer
    /// matching on it is told the geometry is unavailable rather than silently
    /// falling through to another drive's math.
    Omnidirectional,
}

/// The differential-drive geometry, separated from the capabilities realizing it.
///
/// [`KinematicConfig::Differential`] carries the wheel geometry alongside the
/// actuator and encoder lists, but the two directions of the wheel/twist
/// relation depend only on the geometry. They live together here because they
/// are one relation read two ways: a robot whose commanded twist and whose
/// measured twist disagreed about wheel radius would drive one distance and
/// report another, and nothing downstream could detect it. Keeping the pair on
/// one type is what makes them impossible to change independently.
///
/// This is a derived value, not part of the canonical document, so it carries no
/// serde representation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DifferentialDrive {
    /// Driven wheel radius, in metres.
    pub wheel_radius_m: f64,
    /// Distance between the driven wheels, in metres.
    pub wheel_base_m: f64,
}

impl DifferentialDrive {
    /// The geometry with the given wheel radius and track width, both in metres.
    #[must_use]
    pub const fn new(wheel_radius_m: f64, wheel_base_m: f64) -> Self {
        Self {
            wheel_radius_m,
            wheel_base_m,
        }
    }

    /// Check the geometry is usable.
    ///
    /// Both scalars divide in [`Self::wheel_speeds`] and [`Self::body_twist`],
    /// so a zero or non-finite value does not fail loudly - it yields an
    /// infinite or `NaN` wheel command, which is why every consumer must run
    /// this before deriving anything from the geometry.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::KinematicScalar`] when a scalar is not finite and
    /// positive.
    pub fn validate(self) -> Result<Self, ModelError> {
        for (value, field) in [
            (self.wheel_radius_m, KinematicScalarField::WheelRadiusM),
            (self.wheel_base_m, KinematicScalarField::WheelBaseM),
        ] {
            if !(value.is_finite() && value > 0.0) {
                return Err(ModelError::KinematicScalar {
                    kinematics: KinematicKind::Differential,
                    field,
                });
            }
        }
        Ok(self)
    }

    /// The wheel speeds that produce `twist`.
    ///
    /// This is the inverse of [`Self::body_twist`]. `twist.linear_y_mps` is
    /// ignored: a differential drive cannot translate sideways.
    ///
    /// It does not reject a non-finite result: what a caller must do about a
    /// geometry that turns a finite twist into an uncommandable speed depends on
    /// what it is about to do with it, so that judgment stays with the caller.
    #[must_use]
    pub fn wheel_speeds(self, twist: BodyTwist) -> DifferentialWheelSpeeds {
        let half_track = self.wheel_base_m / 2.0;
        let left = twist.linear_x_mps - twist.angular_z_radps * half_track;
        let right = twist.linear_x_mps + twist.angular_z_radps * half_track;
        DifferentialWheelSpeeds {
            left_radps: left / self.wheel_radius_m,
            right_radps: right / self.wheel_radius_m,
        }
    }

    /// The body twist a pair of wheel angular speeds implies.
    ///
    /// The inverse of [`Self::wheel_speeds`]. `linear_y_mps` is always zero.
    #[must_use]
    pub fn body_twist(self, speeds: DifferentialWheelSpeeds) -> BodyTwist {
        let left = speeds.left_radps * self.wheel_radius_m;
        let right = speeds.right_radps * self.wheel_radius_m;
        BodyTwist::planar((left + right) / 2.0, (right - left) / self.wheel_base_m)
    }
}

/// The mecanum-drive geometry: four independently driven wheels with 45-degree
/// rollers, in the standard X configuration.
///
/// Unlike a differential drive this geometry is holonomic, so it realizes
/// `linear_y_mps` directly rather than ignoring it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MecanumDrive {
    /// Driven wheel radius, in metres.
    pub wheel_radius_m: f64,
    /// Front-to-rear axle separation, in metres.
    pub wheel_base_m: f64,
    /// Left-to-right wheel separation, in metres.
    pub track_m: f64,
}

impl MecanumDrive {
    /// The geometry with the given wheel radius, wheel base and track, in metres.
    #[must_use]
    pub const fn new(wheel_radius_m: f64, wheel_base_m: f64, track_m: f64) -> Self {
        Self {
            wheel_radius_m,
            wheel_base_m,
            track_m,
        }
    }

    /// Half the wheel base plus half the track: the lever arm that converts yaw
    /// rate into the differential wheel speed a mecanum uses to rotate.
    const fn yaw_lever_m(self) -> f64 {
        (self.wheel_base_m + self.track_m) / 2.0
    }

    /// Check the geometry is usable.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::KinematicScalar`] when a scalar is not finite and
    /// positive.
    pub fn validate(self) -> Result<Self, ModelError> {
        for (value, field) in [
            (self.wheel_radius_m, KinematicScalarField::WheelRadiusM),
            (self.wheel_base_m, KinematicScalarField::WheelBaseM),
            (self.track_m, KinematicScalarField::TrackM),
        ] {
            if !(value.is_finite() && value > 0.0) {
                return Err(ModelError::KinematicScalar {
                    kinematics: KinematicKind::Mecanum,
                    field,
                });
            }
        }
        Ok(self)
    }

    /// The four wheel speeds that produce `twist`.
    ///
    /// The inverse of [`Self::body_twist`].
    #[must_use]
    pub fn wheel_speeds(self, twist: BodyTwist) -> MecanumWheelSpeeds {
        let yaw = twist.angular_z_radps * self.yaw_lever_m();
        let scale = 1.0 / self.wheel_radius_m;
        MecanumWheelSpeeds {
            front_left_radps: scale * (twist.linear_x_mps - twist.linear_y_mps - yaw),
            front_right_radps: scale * (twist.linear_x_mps + twist.linear_y_mps + yaw),
            rear_left_radps: scale * (twist.linear_x_mps + twist.linear_y_mps - yaw),
            rear_right_radps: scale * (twist.linear_x_mps - twist.linear_y_mps + yaw),
        }
    }

    /// The body twist four wheel angular speeds imply.
    ///
    /// The inverse of [`Self::wheel_speeds`]. Four wheel speeds over-determine a
    /// three-component twist, so this is the least-squares solution: a set of
    /// speeds that no rigid twist can produce (the wheels fighting each other)
    /// yields the twist closest to what they describe rather than an error.
    #[must_use]
    pub fn body_twist(self, speeds: MecanumWheelSpeeds) -> BodyTwist {
        let MecanumWheelSpeeds {
            front_left_radps: fl,
            front_right_radps: fr,
            rear_left_radps: rl,
            rear_right_radps: rr,
        } = speeds;
        BodyTwist::new(
            (fl + fr + rl + rr) * self.wheel_radius_m / 4.0,
            (-fl + fr + rl - rr) * self.wheel_radius_m / 4.0,
            (-fl + fr - rl + rr) * self.wheel_radius_m / (4.0 * self.yaw_lever_m()),
        )
    }
}

/// The Ackermann-steering geometry: one steered axle and one driven axle.
///
/// The relation used is the bicycle model taken at the centre of the driven
/// axle, which is what a single steering actuator can express. `track_m` is
/// carried because the authored document declares it, but a true per-wheel
/// Ackermann split needs two independently steered wheels, which this config
/// does not describe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AckermannDrive {
    /// Front-to-rear axle separation, in metres.
    pub wheel_base_m: f64,
    /// Left-to-right wheel separation, in metres.
    pub track_m: f64,
    /// The largest steering angle the mechanism reaches, in radians.
    pub max_steering_angle_rad: f64,
}

impl AckermannDrive {
    /// The geometry with the given wheel base, track and steering limit.
    #[must_use]
    pub const fn new(wheel_base_m: f64, track_m: f64, max_steering_angle_rad: f64) -> Self {
        Self {
            wheel_base_m,
            track_m,
            max_steering_angle_rad,
        }
    }

    /// Check the geometry is usable.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::KinematicScalar`] when a scalar is not finite and
    /// positive.
    pub fn validate(self) -> Result<Self, ModelError> {
        for (value, field) in [
            (self.wheel_base_m, KinematicScalarField::WheelBaseM),
            (self.track_m, KinematicScalarField::TrackM),
            (
                self.max_steering_angle_rad,
                KinematicScalarField::MaxSteeringAngleRad,
            ),
        ] {
            if !(value.is_finite() && value > 0.0) {
                return Err(ModelError::KinematicScalar {
                    kinematics: KinematicKind::Ackermann,
                    field,
                });
            }
        }
        Ok(self)
    }

    /// The drive speed and steering angle that produce `twist`.
    ///
    /// The inverse of [`Self::body_twist`]. `twist.linear_y_mps` is ignored: a
    /// steered drive cannot translate sideways.
    ///
    /// A stationary robot has no steering angle that produces yaw, so a zero
    /// forward speed yields a zero steering angle. The returned angle is **not**
    /// clamped to [`Self::max_steering_angle_rad`]: a caller that must refuse an
    /// unreachable request needs to see that it was unreachable, which
    /// [`Self::steering_is_reachable`] answers.
    #[must_use]
    pub fn command(self, twist: BodyTwist) -> AckermannCommand {
        let steering_angle_rad = if twist.linear_x_mps == 0.0 {
            0.0
        } else {
            (twist.angular_z_radps * self.wheel_base_m / twist.linear_x_mps).atan()
        };
        AckermannCommand {
            drive_speed_mps: twist.linear_x_mps,
            steering_angle_rad,
        }
    }

    /// The body twist a drive speed and steering angle imply.
    ///
    /// The inverse of [`Self::command`]. `linear_y_mps` is always zero.
    #[must_use]
    pub fn body_twist(self, command: AckermannCommand) -> BodyTwist {
        BodyTwist::planar(
            command.drive_speed_mps,
            command.drive_speed_mps * command.steering_angle_rad.tan() / self.wheel_base_m,
        )
    }

    /// Whether the mechanism can actually reach `steering_angle_rad`.
    #[must_use]
    pub fn steering_is_reachable(self, steering_angle_rad: f64) -> bool {
        steering_angle_rad.abs() <= self.max_steering_angle_rad
    }
}

/// Which drive geometry a [`KinematicConfig`] describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KinematicKind {
    Differential,
    Mecanum,
    Ackermann,
    Omnidirectional,
}

/// Fully normalized runtime-facing robot model.
///
/// This is the whole of what `manifest.json` carries: the robot's identity and
/// structure, the motion it may make, the services it runs, and the components
/// it mounts together with the types behind them. Everything a launched
/// participant needs to know about the robot - including its own configuration -
impl MotionModel {
    #[must_use]
    pub const fn kinematic(&self) -> &KinematicConfig {
        &self.kinematic
    }

    #[must_use]
    pub const fn limits(&self) -> MotionLimits {
        self.limits
    }
}

impl MotionLimits {
    /// Check the envelope is usable.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::MotionLimit`] when a limit is not finite,
    /// positive, and representable as `f32`.
    pub fn validate(self) -> Result<Self, ModelError> {
        for (value, field) in [
            (
                self.max_linear_speed_mps,
                MotionLimitField::MaxLinearSpeedMps,
            ),
            (
                self.max_angular_speed_radps,
                MotionLimitField::MaxAngularSpeedRadps,
            ),
        ] {
            if !(value.is_finite() && value > 0.0 && value <= f64::from(f32::MAX)) {
                return Err(ModelError::MotionLimit { field });
            }
        }
        Ok(self)
    }
}

impl KinematicConfig {
    /// Which drive geometry this configuration describes.
    #[must_use]
    pub const fn kind(&self) -> KinematicKind {
        match self {
            Self::Differential { .. } => KinematicKind::Differential,
            Self::Mecanum { .. } => KinematicKind::Mecanum,
            Self::Ackermann { .. } => KinematicKind::Ackermann,
            Self::Omnidirectional { .. } => KinematicKind::Omnidirectional,
        }
    }
}

impl fmt::Display for KinematicKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Differential => "differential",
            Self::Mecanum => "mecanum",
            Self::Ackermann => "ackermann",
            Self::Omnidirectional => "omnidirectional",
        })
    }
}
impl MotionModel {
    pub(crate) const fn new(kinematic: KinematicConfig, limits: MotionLimits) -> Self {
        Self { kinematic, limits }
    }
}

#[cfg(test)]
mod kinematics_tests {
    use super::{
        AckermannDrive, BodyTwist, DifferentialDrive, DriveKinematics, KinematicConfig,
        KinematicScalarField, MecanumDrive, ModelError,
    };
    use crate::model::identity::CapabilityRef;

    const DIFFERENTIAL: DifferentialDrive = DifferentialDrive::new(0.1, 0.5);
    const MECANUM: MecanumDrive = MecanumDrive::new(0.1, 0.4, 0.6);
    const ACKERMANN: AckermannDrive = AckermannDrive::new(2.5, 1.5, 0.6);

    fn close(left: f64, right: f64, what: &str) {
        assert!((left - right).abs() < 1e-9, "{what}: {left} vs {right}");
    }

    /// Forward and inverse are one relation read two ways. A twist that survives
    /// the round trip is the property that matters: if the two ever disagreed, a
    /// robot would drive one distance and report another, and nothing downstream
    /// could detect it.
    #[test]
    fn a_differential_twist_survives_the_round_trip() {
        for twist in [
            BodyTwist::planar(0.0, 0.0),
            BodyTwist::planar(1.0, 0.0),
            BodyTwist::planar(0.0, 2.0),
            BodyTwist::planar(0.75, -1.25),
        ] {
            let back = DIFFERENTIAL.body_twist(DIFFERENTIAL.wheel_speeds(twist));
            close(back.linear_x_mps, twist.linear_x_mps, "linear x");
            close(back.angular_z_radps, twist.angular_z_radps, "angular z");
            assert_eq!(back.linear_y_mps, 0.0, "a differential drive has no sway");
        }
    }

    #[test]
    fn a_mecanum_twist_survives_the_round_trip_including_sideways() {
        for twist in [
            BodyTwist::new(0.0, 0.0, 0.0),
            BodyTwist::new(1.0, 0.0, 0.0),
            BodyTwist::new(0.0, 1.0, 0.0),
            BodyTwist::new(0.0, 0.0, 1.5),
            BodyTwist::new(0.4, -0.7, 0.9),
        ] {
            let back = MECANUM.body_twist(MECANUM.wheel_speeds(twist));
            close(back.linear_x_mps, twist.linear_x_mps, "linear x");
            close(back.linear_y_mps, twist.linear_y_mps, "linear y");
            close(back.angular_z_radps, twist.angular_z_radps, "angular z");
        }
    }

    #[test]
    fn an_ackermann_twist_survives_the_round_trip() {
        for twist in [
            BodyTwist::planar(1.0, 0.0),
            BodyTwist::planar(2.0, 0.4),
            BodyTwist::planar(-1.5, -0.3),
        ] {
            let back = ACKERMANN.body_twist(ACKERMANN.command(twist));
            close(back.linear_x_mps, twist.linear_x_mps, "linear x");
            close(back.angular_z_radps, twist.angular_z_radps, "angular z");
        }
    }

    #[test]
    fn driving_straight_turns_both_differential_wheels_at_the_same_speed() {
        let speeds = DIFFERENTIAL.wheel_speeds(BodyTwist::planar(1.0, 0.0));
        assert_eq!(speeds.left_radps, speeds.right_radps);
        assert_eq!(speeds.left_radps, 1.0 / DIFFERENTIAL.wheel_radius_m);
    }

    #[test]
    fn turning_in_place_turns_the_differential_wheels_in_opposite_directions() {
        let speeds = DIFFERENTIAL.wheel_speeds(BodyTwist::planar(0.0, 1.0));
        assert_eq!(speeds.left_radps, -speeds.right_radps);
        assert!(
            speeds.right_radps > 0.0,
            "a positive yaw rate drives the right wheel forward"
        );
    }

    /// Strafing left is the motion a differential drive cannot make, so it is
    /// the one that proves the mecanum roller signs are right: the diagonal
    /// pairs must counter-rotate.
    #[test]
    fn strafing_counter_rotates_the_mecanum_diagonals() {
        let speeds = MECANUM.wheel_speeds(BodyTwist::new(0.0, 1.0, 0.0));
        assert_eq!(speeds.front_left_radps, -speeds.front_right_radps);
        assert_eq!(speeds.rear_left_radps, -speeds.rear_right_radps);
        assert_eq!(speeds.front_left_radps, speeds.rear_right_radps);
        assert!(
            speeds.front_right_radps > 0.0,
            "left sway drives FR forward"
        );
    }

    /// A non-holonomic geometry ignores sway rather than approximating it, so a
    /// sideways request must not leak into the wheels.
    #[test]
    fn non_holonomic_geometries_ignore_a_sideways_request() {
        let straight = BodyTwist::planar(1.0, 0.0);
        let swaying = BodyTwist::new(1.0, 5.0, 0.0);
        assert_eq!(
            DIFFERENTIAL.wheel_speeds(straight),
            DIFFERENTIAL.wheel_speeds(swaying)
        );
        assert_eq!(ACKERMANN.command(straight), ACKERMANN.command(swaying));
    }

    /// A stationary robot has no steering angle that produces yaw, so asking for
    /// one must not divide by zero into a `NaN` the caller would then command.
    #[test]
    fn a_stationary_ackermann_has_a_defined_steering_angle() {
        let command = ACKERMANN.command(BodyTwist::planar(0.0, 1.0));
        assert_eq!(command.drive_speed_mps, 0.0);
        assert_eq!(command.steering_angle_rad, 0.0);
    }

    #[test]
    fn the_steering_limit_is_reported_rather_than_silently_clamped() {
        let command = ACKERMANN.command(BodyTwist::planar(0.5, 2.0));
        assert!(
            command.steering_angle_rad.abs() > ACKERMANN.max_steering_angle_rad,
            "this request should exceed the mechanism"
        );
        assert!(!ACKERMANN.steering_is_reachable(command.steering_angle_rad));
        assert!(ACKERMANN.steering_is_reachable(0.0));
    }

    fn reference() -> CapabilityRef {
        "base.motor".parse().expect("a well formed capability ref")
    }

    #[test]
    fn every_authored_geometry_resolves_to_its_kinematics() {
        let differential = KinematicConfig::Differential {
            left_actuators: vec![reference()],
            right_actuators: vec![reference()],
            left_encoders: Vec::new(),
            right_encoders: Vec::new(),
            wheel_radius_m: 0.1,
            wheel_base_m: 0.5,
        };
        assert_eq!(
            differential.drive_kinematics().expect("valid geometry"),
            DriveKinematics::Differential(DIFFERENTIAL)
        );

        let mecanum = KinematicConfig::Mecanum {
            front_left_actuator: reference(),
            front_right_actuator: reference(),
            rear_left_actuator: reference(),
            rear_right_actuator: reference(),
            wheel_radius_m: 0.1,
            wheel_base_m: 0.4,
            track_m: 0.6,
        };
        assert_eq!(
            mecanum.drive_kinematics().expect("valid geometry"),
            DriveKinematics::Mecanum(MECANUM)
        );

        let ackermann = KinematicConfig::Ackermann {
            steering_actuator: reference(),
            drive_actuator: reference(),
            steering_encoder: None,
            drive_encoder: None,
            wheel_base_m: 2.5,
            track_m: 1.5,
            max_steering_angle_rad: 0.6,
        };
        assert_eq!(
            ackermann.drive_kinematics().expect("valid geometry"),
            DriveKinematics::Ackermann(ACKERMANN)
        );

        // An omnidirectional document authors actuators and encoders but no
        // geometry, so there is nothing to resolve and the variant says so
        // rather than borrowing another drive's math.
        let omnidirectional = KinematicConfig::Omnidirectional {
            actuators: vec![reference()],
            encoders: Vec::new(),
        };
        assert_eq!(
            omnidirectional
                .drive_kinematics()
                .expect("carries no scalars to reject"),
            DriveKinematics::Omnidirectional
        );
    }

    #[test]
    fn a_non_positive_scalar_is_refused_by_the_geometry_it_belongs_to() {
        assert!(matches!(
            DifferentialDrive::new(0.0, 0.5).validate(),
            Err(ModelError::KinematicScalar {
                field: KinematicScalarField::WheelRadiusM,
                ..
            })
        ));
        assert!(matches!(
            MecanumDrive::new(0.1, 0.4, f64::NAN).validate(),
            Err(ModelError::KinematicScalar {
                field: KinematicScalarField::TrackM,
                ..
            })
        ));
        assert!(matches!(
            AckermannDrive::new(2.5, 1.5, -0.1).validate(),
            Err(ModelError::KinematicScalar {
                field: KinematicScalarField::MaxSteeringAngleRad,
                ..
            })
        ));
    }
}
