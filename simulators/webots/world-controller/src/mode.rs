use super::*;

pub(super) fn poll_while_paused(
    webots: &Webots,
    supervisor: &Supervisor,
    link: &ControllerLink,
) -> Result<()> {
    synchronize_control(webots)?;
    validate_paused_mode(supervisor, link)?;
    link.exchange(ControllerEvent::Heartbeat)?;
    std::thread::sleep(PAUSED_POLL);
    Ok(())
}

pub(super) fn validate_paused_mode(supervisor: &Supervisor, link: &ControllerLink) -> Result<()> {
    let raw = supervisor.simulation_get_mode()?;
    match map_mode(raw) {
        Some(ObservedNativeMode::Paused) => Ok(()),
        Some(observed @ (ObservedNativeMode::Run | ObservedNativeMode::Fast)) => {
            link.exchange(ControllerEvent::Fault(ControllerFault::UnsupportedMode {
                observed,
            }))?;
            bail!("Webots entered unsupported native mode {observed:?} while paused")
        }
        Some(observed) => {
            link.exchange(ControllerEvent::Fault(ControllerFault::Protocol {
                detail: format!("expected PAUSE outside wb_robot_step, observed {observed:?}"),
            }))?;
            bail!("Webots left PAUSE without host authority")
        }
        None => {
            link.exchange(ControllerEvent::Fault(ControllerFault::UnsupportedMode {
                observed: ObservedNativeMode::Run,
            }))?;
            bail!("Webots returned unknown simulation mode {raw} while paused")
        }
    }
}

pub(super) fn set_motion(
    webots: &Webots,
    supervisor: &Supervisor,
    motion: NativeMotion,
) -> Result<()> {
    supervisor
        .simulation_set_mode(match motion {
            NativeMotion::Paused => WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_PAUSE,
            NativeMotion::RealTime => WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_REAL_TIME,
        })
        .context("failed to change Webots simulation mode")?;
    synchronize_control(webots)
}

pub(super) fn synchronize_control(webots: &Webots) -> Result<()> {
    // R2025a may return the cached previous mode after simulation_set_mode.
    // A zero-duration step refreshes control state without advancing physics,
    // including while paused. A positive-duration step would block in PAUSE.
    let before = webots.get_time()?;
    ensure!(
        webots.step(0)?,
        "Webots stopped during control synchronization"
    );
    ensure!(
        webots.get_time()? == before,
        "Webots advanced physics during control synchronization"
    );
    Ok(())
}

pub(super) fn validate_native_mode(supervisor: &Supervisor, link: &ControllerLink) -> Result<()> {
    let raw = supervisor.simulation_get_mode()?;
    match map_mode(raw) {
        Some(ObservedNativeMode::RealTime) => Ok(()),
        Some(observed @ (ObservedNativeMode::Run | ObservedNativeMode::Fast)) => {
            link.exchange(ControllerEvent::Fault(ControllerFault::UnsupportedMode {
                observed,
            }))?;
            bail!("Webots entered unsupported native mode {observed:?}")
        }
        Some(observed) => {
            link.exchange(ControllerEvent::Fault(ControllerFault::Protocol {
                detail: format!("expected REAL_TIME before a native step, observed {observed:?}"),
            }))?;
            bail!("Webots was not in REAL_TIME before a native step")
        }
        None => {
            link.exchange(ControllerEvent::Fault(ControllerFault::UnsupportedMode {
                observed: ObservedNativeMode::Run,
            }))?;
            bail!("Webots returned unknown simulation mode {raw}")
        }
    }
}

pub(super) const fn map_mode(mode: WbSimulationMode) -> Option<ObservedNativeMode> {
    if mode == WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_PAUSE {
        Some(ObservedNativeMode::Paused)
    } else if mode == WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_REAL_TIME {
        Some(ObservedNativeMode::RealTime)
    } else if mode == WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_FAST {
        Some(ObservedNativeMode::Fast)
    } else {
        None
    }
}

pub(super) const fn observed_mode(motion: NativeMotion) -> ObservedNativeMode {
    match motion {
        NativeMotion::Paused => ObservedNativeMode::Paused,
        NativeMotion::RealTime => ObservedNativeMode::RealTime,
    }
}
