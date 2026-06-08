pub mod capability;
mod component;
pub mod conformance;
mod driver;
pub mod localize_backend;
mod motion;
pub mod profile;
pub mod resolver;
mod robot;
pub mod role;
pub mod role_resolution;
pub mod transform;
mod validation;

pub use component::Component;
pub use driver::{ConnectionConfig, DriverConfig, GpioDirection, GpioPinConfig};
pub use localize_backend::{
    LocalizeBackendKind, ResolvedLocalizeBackend, resolve_localize_backend,
};
pub use motion::{KinematicConfig, KinematicKind, Motion};
pub use profile::{AutonomyProfileId, AutonomyProfileSpec, autonomy_profile};
pub use resolver::{ResolvedCapabilityRole, ResolvedFacts, SourceBundle, resolve_source_bundle};
pub use robot::{
    ComponentSource, Components, Identity, Network, NetworkTls, Phoxal, PhoxalRuntimes,
    PlatformRuntimeOverride, Robot, SourceGit, SourcePath, Tool, Uplink, UserRuntime,
    ValidationError,
};
pub use role::Role;
pub use role_resolution::{RoleAssignment, RoleResolution};
