//! Negative fixture: scenario test functions cannot be generic.

#![cfg(feature = "scenario")]

use phoxal::scenario::Simulation;

#[phoxal::scenario]
fn generic_scenario<T>(sim: &mut Simulation) -> phoxal::Result<()> {
    let _ = (sim, std::marker::PhantomData::<T>);
    Ok(())
}

fn main() {}
