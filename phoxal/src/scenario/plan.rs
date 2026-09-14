//! Placeholder `ScenarioPlan` and `Step` types for P1.
//!
//! P2 (`plan.rs`, `action.rs`, `step.rs`, `program.rs`) replaces these
//! with the real typed plan, finite-bound normalization, validated action
//! schedule, and capture declarations. The shape must already match the
//! planned trait so attribute code can compile today.

use std::path::PathBuf;
use std::time::Duration;

/// One finite simulated experiment. P1 stub: holds the scene path and the
/// requested duration only. P2 expands this with the action schedule and
/// capture declarations.
#[derive(Debug, Clone)]
pub struct ScenarioPlan {
    pub scene: PathBuf,
    pub duration: Duration,
}

impl ScenarioPlan {
    pub fn new(scene: impl Into<PathBuf>, duration: Duration) -> Self {
        Self {
            scene: scene.into(),
            duration,
        }
    }
}
