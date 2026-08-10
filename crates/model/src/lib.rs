//! Canonical immutable runtime robot model.
//!
//! The model is constructed by source/build tooling or decoded from the
//! validated runtime bundle; its wire shape is owned by `phoxal-bundle`.
//!
//! # Paths
//!
//! Concepts live in the module that owns them: [`asset`], [`builder`],
//! [`component`], [`identity`], [`robot`], [`simulation`], [`structure`].
//!
//! The crate root is a deliberate facade over the handful of names a consumer
//! meets first, so that loading, reading or composing a robot does not require
//! learning the module layout. Those names - and the error vocabulary, whose
//! module is private precisely so the root is its only path - are the canonical
//! ones; write `phoxal_model::Robot`, not `phoxal_model::robot::Robot`.
//! Everything else is named through its owning module.
//!
//! Modules inside this crate import from the owning module, never through this
//! facade.

mod error;

pub mod asset;
pub mod builder;
pub mod component;
pub mod footprint;
pub mod identity;
pub mod robot;
pub mod simulation;
pub mod structure;

#[doc(hidden)]
pub mod compiler;

pub use asset::AssetId;
pub use builder::RobotBuilder;
pub use component::capability::CapabilityRole;
pub use error::{
    IdentifierKind, JointOwner, KinematicScalarField, LinkRole, ModelError, MotionLimitField,
    PoseOwner, StructureError,
};
pub use footprint::FootprintEnvelope;
pub use robot::{Clock, Robot};
