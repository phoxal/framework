//! A scenario is an ordinary executable with an explicit scene.

fn main() -> phoxal::Result<()> {
    let mut simulation = phoxal::scenario::Simulation::new("simulation/scene.xml")?;
    let mut plan = simulation.plan();
    plan.wait_steps(1)?;
    Ok(())
}
