//! Canonical immutable runtime robot model.
//!
//! The model is constructed from finalized sources at load time; it has no
//! persisted wire form of its own.
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
pub mod identity;
pub mod robot;
pub mod simulation;
pub mod structure;

#[doc(hidden)]
pub mod compiler;

pub use asset::{AssetError, AssetId, ParticipantAssetResolver};
pub use builder::RobotBuilder;
pub use error::{
    IdentifierKind, JointOwner, KinematicScalarField, LinkRole, ModelError, MotionLimitField,
    PoseOwner, StructureError,
};
pub use robot::{Clock, Robot, RobotIdentity};
