//! Typed bus wire contracts.
//!
//! Every cross-process payload, query, or command is a typed contract here.
//! One module per platform runtime (plus `component` for component
//! capabilities, `presence` for the engine-owned liveness contract, and
//! `simulation` for the simulator protocol). Contract versions evolve inside
//! `pub mod vN` submodules.

pub mod v1;

pub mod component;
pub mod presence;
pub mod simulation;

pub mod asset;
pub mod drive;
pub mod explore;
pub mod follow;
pub mod frame;
pub mod joint;
pub mod localize;
pub mod map;
pub mod mission;
pub mod motion;
pub mod odometry;
pub mod perception;
pub mod plan;
pub mod power;
pub mod safety;
pub mod video;
