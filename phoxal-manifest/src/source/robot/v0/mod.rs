//! Exact `robot.yaml` v0 document and its file-local validation types.

pub mod capability;
mod component;
mod driver;
mod manifest;
mod motion;
pub mod role;
mod validation;

pub use component::Component;
pub use driver::{ConnectionConfig, DriverConfig, GpioDirection, GpioPinConfig};
pub use manifest::{
    BehaviorConfig, Manifest, RobotSection, Router, Schema, UserService, UserTool, ValidationError,
};
pub use motion::{CapabilityRef, KinematicConfig, MotionLimits};
pub use role::Role;
