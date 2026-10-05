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

    /// One scalar actuator command; its destination is the authored connection.
    pub struct ActuatorCommand {
        /// Explicit control law, required by actuator admission.
        #[phoxal(tag = 1)]
        pub control: Option<Control>,
    }
}

pub use v1::*;

#[cfg(test)]
mod tests {
    use prost::Name;

    use super::{ActuatorCommand, Control};
    use crate::schema::MessageSchema;

    #[test]
    fn retains_public_protobuf_identity_and_schema() {
        assert_eq!(ActuatorCommand::PACKAGE, "phoxal.component.actuator.v1");
        assert_eq!(ActuatorCommand::NAME, "ActuatorCommand");
        assert_eq!(
            ActuatorCommand::WIRE_NAME,
            "phoxal.component.actuator.v1.ActuatorCommand"
        );
        assert!(ActuatorCommand::retain_schema() > 0);
        assert!(<Control as MessageSchema>::retain_schema() > 0);
    }
}
