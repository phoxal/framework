//! Positive fixture: a function-based scenario remains an ordinary test.

#![cfg(feature = "scenario")]

#[phoxal::scenario]
fn valid_scenario(sim: &mut phoxal::scenario::Simulation) -> phoxal::Result<()> {
    let mut plan = sim.plan();
    plan.wait_steps(1)?;
    Ok(())
}

#[ignore = "ordinary Rust test attribute is preserved"]
#[phoxal::scenario]
fn ignored_scenario(sim: &mut phoxal::scenario::Simulation) -> phoxal::Result<()> {
    let mut plan = sim.plan();
    plan.wait_steps(1)?;
    Ok(())
}

#[cfg(any())]
#[phoxal::scenario]
fn configured_out_scenario(
    _sim: &mut phoxal::scenario::Simulation,
) -> phoxal::Result<()> {
    Ok(())
}

#[test]
fn ordinary_test_can_construct_the_same_fixture_explicitly() {
    let _ = phoxal::scenario::Simulation::from_context("ordinary_test").unwrap();
}

fn main() {}
