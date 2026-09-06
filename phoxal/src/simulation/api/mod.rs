//! The `simulation` contract family: passive progress from an attached world.
//!
//! The per-robot simulator controller publishes one [`StepEvent`] after it has
//! admitted the outputs for a completed native transition. The event records
//! producer-local order only. It never advances participant scheduling or
//! claims that another subscriber has observed the corresponding outputs.

crate::nodes! {
    family Simulation;

    step;
}

pub use step::StepEvent;
