//! Curated canonical robot model.

pub use phoxal_model::robot::ComponentInstance;
pub use phoxal_model::{Robot, component, simulation, structure};

/// Canonical robot identity and motion vocabulary.
pub mod robot {
    pub use phoxal_model::robot::{
        ComponentInstance, KinematicConfig, MotionLimits, MotionModel, ROBOT_SCHEMA, RobotIdentity,
    };
}
