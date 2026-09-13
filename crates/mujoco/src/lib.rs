//! Native MuJoCo integration owned by the independent simulator application.
//!
//! The default build contains only closed-model artifact validation and digesting.
//! Enable the `native` feature to compile MJCF with the pinned `mujoco-rs`
//! binding and use the native `Model`, `Workspace`, and `Scene` types.
//!
//! `Scene` is the sole owner of authoritative mutable physics data exposed by
//! this crate.
//! `Workspace` is an explicitly non-authoritative computation owner suitable
//! for a service that has chosen native model access.
//! `Model` is immutable after compilation and can be shared with read-only
//! consumers.

mod artifact;
mod error;

#[cfg(feature = "native")]
mod composition;
#[cfg(feature = "native")]
mod model;
#[cfg(feature = "native")]
mod provider;
#[cfg(feature = "native")]
mod scene;

pub use artifact::{ClosedModel, Resource, ResourceLimits};
pub use error::ArtifactError;

#[cfg(feature = "native")]
pub use composition::{
    ComponentAttachment, CompositionError, ModelComposition, NAMESPACE_SEPARATOR, SceneComposition,
    compose_model, compose_scene, unique_direct_root_body,
};
#[cfg(feature = "native")]
pub use error::{ModelError, SceneError, WorkspaceError};
#[cfg(feature = "native")]
pub use model::{
    ActuatorBinding, ActuatorHandle, ActuatorInfo, ActuatorMode, BodyHandle, BodyInfo,
    CameraBinding, CameraHandle, CameraInfo, JointHandle, JointInfo, JointKind, Model, ModelCounts,
    ModelHandle, ModelIdentity, ObjectKind, SensorBinding, SensorHandle, SensorInfo, SensorKind,
    SiteBinding, SiteHandle, SiteInfo,
};
#[cfg(feature = "native")]
pub use provider::{
    ActuatorSelection, Boundary, ControlledError, ControlledPhase, ControlledScene, ControlledStep,
    ExecutionId, HoldProvider, ObservationReceipt, PrepareRequest, ProviderReset,
    SimulationProvider, SourceId, TimelineId,
};
#[cfg(feature = "native")]
pub use scene::{PhysicsQuantum, Scene, ScenePhase, SceneStep, StateSnapshot, Workspace};
#[cfg(feature = "rendering")]
pub use scene::{RenderedCamera, ViewCamera};
