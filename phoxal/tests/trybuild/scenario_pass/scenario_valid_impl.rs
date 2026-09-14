//! Positive trybuild fixture: a valid `impl Scenario for ConcreteType` block
//! compiles when the `scenario` feature is enabled.

#![cfg(feature = "scenario")]

use phoxal::scenario::{Scenario, ScenarioPlan, ScenarioRun};
use std::time::Duration;

#[derive(Default)]
pub struct ValidScenario;

#[phoxal::scenario]
impl Scenario for ValidScenario {
    fn plan(&self) -> phoxal::Result<ScenarioPlan> {
        Ok(ScenarioPlan::new("simulation/scene.xml", Duration::from_secs(1)))
    }
    fn verify(&self, _run: &ScenarioRun) -> phoxal::Result<()> {
        Ok(())
    }
}

fn main() {}
