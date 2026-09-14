//! Negative trybuild fixture: `#[phoxal::scenario]` on an inherent impl must
//! be rejected with a clear diagnostic.

#![cfg(feature = "scenario")]

use phoxal::scenario::{Scenario, ScenarioPlan, ScenarioRun};
use std::time::Duration;

#[derive(Default)]
pub struct Inherent;

#[phoxal::scenario]
impl Inherent {
    fn plan(&self) -> phoxal::Result<ScenarioPlan> {
        Ok(ScenarioPlan::new("x", Duration::from_secs(1)))
    }
    fn verify(&self, _run: &ScenarioRun) -> phoxal::Result<()> {
        Ok(())
    }
}

fn main() {}
