//! Negative fixture: scenario test functions require a mutable fixture.

#![cfg(feature = "scenario")]

use phoxal::scenario::Simulation;

#[phoxal::scenario]
fn immutable_fixture(_sim: &Simulation) -> phoxal::Result<()> {
    Ok(())
}

fn main() {}
