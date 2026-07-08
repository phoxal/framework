//! Authored manifest model.
//!
//! The single source of truth for what a robot is: `robot.yaml`,
//! `structure.urdf`, and `components/<name>/component.yaml` /
//! `simulation.yaml`. Each submodule is the public schema for one authored
//! file kind; runtimes and the CLI parse these directly.

pub mod component;
pub mod robot;
pub mod simulation;
pub mod structure;
pub mod v0;
