//! The `simulation` contract family: progress published by an attached world.
//!
//! The simulator host owns production of this clock after the supervisor has
//! attached it to an execution. The supervisor owns the current time domain;
//! this family says only which completed world step has become observable.

crate::nodes! {
    family Simulation;

    clock;
}

pub use clock::Clock;
