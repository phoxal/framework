use super::*;

pub(super) fn apply_mutation(supervisor: &Supervisor, mutation: NativeMutation) -> Result<()> {
    ensure!(
        supervisor.simulation_get_mode()? == WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_PAUSE,
        "native scene mutation is permitted only while Webots is paused"
    );
    match mutation {
        NativeMutation::StartRobotController { .. } => {
            bail!("controller bootstrap is not a scene mutation")
        }
        NativeMutation::ImportRobot {
            definition, source, ..
        } => {
            ensure!(
                Node::from_def(&definition).is_err(),
                "Robot DEF {definition} already exists"
            );
            supervisor
                .get_root()?
                .field("children")?
                .import_mf_node_from_string(-1, &source)?;
            let imported = Node::from_def(&definition)
                .with_context(|| format!("imported Robot DEF {definition} is not addressable"))?;
            ensure!(
                imported.base_type_name()? == "Robot",
                "imported DEF {definition} is not a Robot"
            );
        }
        NativeMutation::RemoveRobot { definition, .. } => {
            Node::from_def(&definition)
                .with_context(|| format!("Robot DEF {definition} is absent during removal"))?
                .remove()?;
            ensure!(
                Node::from_def(&definition).is_err(),
                "Robot DEF {definition} remained after removal"
            );
        }
        NativeMutation::RollbackRobot { definition, .. } => {
            if let Ok(node) = Node::from_def(&definition) {
                node.remove()?;
            }
            ensure!(
                Node::from_def(&definition).is_err(),
                "Robot DEF {definition} remained after rollback"
            );
        }
    }
    Ok(())
}

pub(super) fn start_imported_controller(
    webots: &Webots,
    supervisor: &Supervisor,
    link: &ControllerLink,
    transaction: u64,
) -> Result<()> {
    // R2025a starts imported controllers from its running event loop. Zero-duration
    // controller requests do not authorize physics, so bootstrap can preserve this boundary.
    // The installed-runtime proof covers startup and return to PAUSE without a time change.
    let before = webots.get_time()?;
    set_motion(webots, supervisor, NativeMotion::RealTime)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    let result = (|| -> Result<()> {
        loop {
            synchronize_control(webots)?;
            ensure!(
                webots.get_time()? == before,
                "controller bootstrap advanced native physics"
            );
            validate_native_mode(supervisor, link)?;
            match link.directive()? {
                HostDirective::Mutate(NativeMutation::StartRobotController {
                    transaction: current,
                    ready,
                    ..
                }) if current == transaction => {
                    if ready {
                        return Ok(());
                    }
                }
                directive => bail!("native import bootstrap lost authority: {directive:?}"),
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "imported controller bootstrap timed out"
            );
            link.exchange(ControllerEvent::Heartbeat)?;
            std::thread::sleep(PAUSED_POLL);
        }
    })();
    set_motion(webots, supervisor, NativeMotion::Paused)?;
    ensure!(
        webots.get_time()? == before,
        "controller bootstrap changed the paused boundary"
    );
    result
}
