//! Canonical immutable runtime robot model.
//!
//! The model is constructed by source/build tooling and decoded from the
//! bundle's `manifest.json`. [`manifest`] owns that document: its schema tag
//! and the envelope around the [`Robot`] body, and it lives here rather than in
//! `phoxal::bundle` so the compiler that writes it and the reader that loads it
//! both reach it through the module that owns the body. `phoxal::bundle` owns
//! where the file sits and how it is read; this module owns no filesystem layout
//! at all, and the authored project documents are `phoxal::authoring`.
//!
//! [`Robot`] is the whole of a compiled robot: its identity and structure, the
//! motion it may make, the `services` it runs with their configuration, the
//! `components` it mounts, and the `component_types` behind them with the
//! simulation folded into each. [`Robot::component`](robot::Robot::component)
//! joins an instance with its type and simulation, so no consumer joins those
//! maps by hand.
//!
//! # Paths
//!
//! Concepts live in the module that owns them: [`asset`], [`builder`],
//! [`component`], [`connection`], [`identity`], [`manifest`], [`robot`],
//! [`simulation`], [`structure`].
//!
//! This module's root is a deliberate facade over the handful of names a consumer
//! meets first, so that loading, reading or composing a robot does not require
//! learning the module layout. Those names - and the error vocabulary, whose
//! module is private precisely so the root is its only path - are the canonical
//! ones; write `phoxal::model::Robot`, not `phoxal::model::robot::Robot`.
//! Everything else is named through its owning module.
//!
//! Code inside the framework imports from the owning module, never through this
//! facade.

mod error;

pub mod asset;
pub mod builder;
pub mod component;
pub mod connection;
pub mod footprint;
pub mod identity;
pub mod manifest;
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
pub use manifest::ManifestDocument;
pub use robot::Robot;
