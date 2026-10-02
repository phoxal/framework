//! Negative fixture: scenario test functions require a Simulation fixture.

#![cfg(feature = "scenario")]

#[phoxal::scenario]
fn wrong_fixture(_value: &mut String) -> phoxal::Result<()> {
    Ok(())
}

fn main() {}
