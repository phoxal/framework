//! The standard actuator vocabulary.
//!
//! Contains only the capability-standard actuator command surface: the
//! messages a component's declared `motor` capability accepts. They carry
//! no product-specific intent, status, or constraint semantics; those
//! belong to the services that own them.

#[phoxal::messages(package = "phoxal.component.actuator.v1")]
mod v1 {
    /// The actuator control law of one target: a payload enum.
    pub enum Control {
        /// Direct wheel velocity in radians per second.
        #[phoxal(tag = 2)]
        VelocityRadps(f64),
        /// Direct wheel torque in newton-meters.
        #[phoxal(tag = 3)]
        TorqueNm(f64),
    }

    /// One addressed actuator command.
    pub struct ActuatorTarget {
        /// The actuator's stable instance-qualified name.
        #[phoxal(tag = 1)]
        pub actuator_id: String,
        /// The control law applied to this target.
        #[phoxal(tag = 2)]
        pub control: Option<Control>,
    }

    /// A complete set of actuator commands for one control period.
    pub struct ActuatorSetpoint {
        /// Every addressed target of this setpoint.
        #[phoxal(tag = 1)]
        pub targets: Vec<ActuatorTarget>,
    }
}

pub use v1::*;

#[cfg(test)]
mod tests {
    use prost::Name;

    use super::{ActuatorSetpoint, ActuatorTarget, Control};
    use crate::schema::MessageSchema;

    #[test]
    fn retains_public_protobuf_identity_and_schema() {
        assert_eq!(ActuatorSetpoint::PACKAGE, "phoxal.component.actuator.v1");
        assert_eq!(ActuatorSetpoint::NAME, "ActuatorSetpoint");
        assert_eq!(
            ActuatorSetpoint::WIRE_NAME,
            "phoxal.component.actuator.v1.ActuatorSetpoint"
        );
        assert!(ActuatorSetpoint::retain_schema() > 0);
        assert!(ActuatorTarget::retain_schema() > 0);
        assert!(<Control as MessageSchema>::retain_schema() > 0);
    }
}
