use super::*;

pub(super) const PARKED_POLL: Duration = Duration::from_millis(10);

#[allow(
    clippy::too_many_arguments,
    reason = "fault convergence retains the exact native boundary and any issued transition"
)]
pub(super) async fn park_after_cooperative_failure(
    devices: &mut DeviceSet,
    webots: &Webots,
    link: &ControllerLink,
    event: ControllerEvent,
    mut progress: WorldProgress,
    mut motion: NativeMotion,
    mut pending_native_entry: bool,
    step_ms: i32,
) -> Result<()> {
    // A controller that cannot park is not isolated and must disconnect so the host classifies
    // the synchronization role as world-fatal. Once parked, stay outside `wb_robot_step` and keep
    // driving the private request/response link until the host retires this one Robot.
    devices.invalidate_and_park()?;
    synchronize_devices(webots)?;
    link.exchange(event)?;
    // A peer may already have entered the next synchronized quantum. Finish every
    // previously issued native transition with parked actuators, without publishing output,
    // until the common boundary selects PAUSE. Leaving that barrier early would strand peers.
    loop {
        if pending_native_entry {
            ensure!(
                webots.step(step_ms)?,
                "Webots stopped before cooperative parking completed"
            );
            progress = observed_progress(webots.get_time()?, u64::try_from(step_ms)? * 1_000_000)?;
            motion = NativeMotion::RealTime;
        }
        link.exchange(ControllerEvent::RobotBoundary { progress, motion })?;
        match link.directive()? {
            HostDirective::Continue {
                motion: NativeMotion::RealTime,
            } => pending_native_entry = true,
            HostDirective::Continue {
                motion: NativeMotion::Paused,
            }
            | HostDirective::Park => break,
            HostDirective::Stop { .. } => break,
            HostDirective::Mutate(_) => bail!("world mutation directed to parking Robot"),
        }
    }
    link.exchange(ControllerEvent::RobotParked)?;
    loop {
        match link.directive()? {
            HostDirective::Stop { reason } => {
                tracing::info!(%reason, "retiring the cooperatively parked Robot");
                link.exchange(ControllerEvent::Stopped)?;
                return Ok(());
            }
            HostDirective::Continue { .. } | HostDirective::Park => {
                link.exchange(ControllerEvent::Heartbeat)?;
                tokio::time::sleep(PARKED_POLL).await;
            }
            HostDirective::Mutate(_) => {
                bail!("the host sent a world-only scene mutation to a parked Robot controller");
            }
        }
    }
}
