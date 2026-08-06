//! Curated canonical robot model.

pub use phoxal_model::robot::ComponentInstance;
pub use phoxal_model::{Clock, Robot, component, simulation, structure};

/// Canonical robot identity and motion vocabulary.
pub mod robot {
    pub use phoxal_model::robot::{
        Clock, ComponentInstance, KinematicConfig, MotionLimits, MotionModel, RobotIdentity,
    };
}
