//! Negative trybuild fixture: the attribute must reject generic impls.

#![cfg(feature = "scenario")]

use phoxal::scenario::{Scenario, ScenarioPlan, ScenarioRun};
use std::time::Duration;

#[derive(Default)]
pub struct GenericScenario<T: Default + 'static>(pub T);

#[phoxal::scenario]
impl<T: Default + 'static> Scenario for GenericScenario<T> {
    fn plan(&self) -> phoxal::Result<ScenarioPlan> {
        Ok(ScenarioPlan::new("x", Duration::from_secs(1)))
    }
    fn verify(&self, _run: &ScenarioRun) -> phoxal::Result<()> {
        Ok(())
    }
}

fn main() {}
