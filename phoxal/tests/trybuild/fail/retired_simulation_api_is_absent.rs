use phoxal::api;

fn main() {
    let _ = std::mem::size_of::<api::simulation::RobotSpawn>();
    let _ = std::mem::size_of::<api::simulation::SpawnRequest>();
    let _ = std::mem::size_of::<api::simulation::SpawnSet>();
    let _ = std::mem::size_of::<api::simulation::Control>();
    let _ = std::mem::size_of::<api::simulation::RobotPose>();
    let _ = std::mem::size_of::<api::simulation::Contact>();

    let simulation = api::topic::new().simulation();
    let _ = simulation.spawn();
    let _ = simulation.control();
    let _ = simulation.robot_pose();
    let _ = simulation.contact();
}
