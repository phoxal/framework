//! Negative trybuild fixture: the attribute must reject impl blocks whose
//! trait is not `Scenario`.

#![cfg(feature = "scenario")]

pub trait NotScenario {
    fn plan(&self) -> phoxal::Result<()>;
}

#[derive(Default)]
pub struct Wrong;

#[phoxal::scenario]
impl NotScenario for Wrong {
    fn plan(&self) -> phoxal::Result<()> {
        Ok(())
    }
}

fn main() {}
