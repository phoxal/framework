use super::*;

pub(super) struct MotorDevice {
    pub(super) capability: phoxal::model::identity::CapabilityRef,
    pub(super) native: webots_rs::device::motor::Motor,
    pub(super) command: MotorCommand,
    pub(super) gear_ratio: f64,
    pub(super) position_velocity: f64,
    pub(super) receiver: LiveSetpointReceiver<api::component::motor::Command>,
    pub(super) authority: FixedSourceLease<api::component::motor::Command>,
    pub(super) ready: ParticipantReadyEvents,
}

/// Complete every independent stop attempt before returning their aggregate failure.
pub(crate) fn stop_every<T>(
    targets: &mut [T],
    label: impl Fn(&T) -> String,
    mut stop: impl FnMut(&mut T) -> Result<()>,
) -> Result<()> {
    let mut failures = Vec::new();
    for target in targets {
        let target_label = label(target);
        if let Err(error) = stop(target) {
            failures.push(format!("{target_label}: {error:#}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!("failed to stop native motors: {}", failures.join("; "));
    }
}

pub(crate) const fn classify_selection(
    selected: bool,
    accepted_new: bool,
    held_before_selection: bool,
    ready_count: usize,
    had_offers: bool,
) -> ActuationSelection {
    if selected {
        return if accepted_new {
            ActuationSelection::SelectedNew
        } else {
            ActuationSelection::Reused
        };
    }
    ActuationSelection::None {
        reason: if ready_count == 0 {
            NoActuationReason::SourceAbsent
        } else if ready_count > 1 {
            NoActuationReason::SourceConflict
        } else if held_before_selection {
            NoActuationReason::Expired
        } else if had_offers {
            NoActuationReason::Rejected
        } else {
            NoActuationReason::Missing
        },
    }
}

impl MotorDevice {
    pub(super) fn apply_action(&self, action: MotorAction) -> Result<()> {
        match action {
            MotorAction::Position(value) => {
                self.native.set_velocity(self.position_velocity)?;
                self.native.set_position(value)?;
            }
            MotorAction::Velocity(value) => {
                self.native.set_position(f64::INFINITY)?;
                self.native.set_velocity(value)?;
            }
            MotorAction::Torque(value) => {
                self.native.set_position(f64::INFINITY)?;
                self.native.set_torque(value)?;
            }
            MotorAction::Stop => self.stop()?,
        }
        Ok(())
    }

    pub(super) fn stop(&self) -> Result<()> {
        match self.command {
            MotorCommand::Position => self.native.set_velocity(0.0)?,
            MotorCommand::Velocity => {
                self.native.set_position(f64::INFINITY)?;
                self.native.set_velocity(0.0)?;
            }
            MotorCommand::Torque => {
                self.native.set_position(f64::INFINITY)?;
                self.native.set_torque(0.0)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MotorAction {
    Position(f64),
    Velocity(f64),
    Torque(f64),
    Stop,
}

impl From<MotorAction> for AppliedActuation {
    fn from(action: MotorAction) -> Self {
        match action {
            MotorAction::Position(value) => Self::Position(value),
            MotorAction::Velocity(value) => Self::Velocity(value),
            MotorAction::Torque(value) => Self::Torque(value),
            MotorAction::Stop => Self::Stop,
        }
    }
}

pub(crate) fn dispatch_motor(
    configured: MotorCommand,
    command: &api::component::motor::Command,
    gear_ratio: f64,
) -> Result<MotorAction> {
    if matches!(command, api::component::motor::Command::Stop) {
        return Ok(MotorAction::Stop);
    }
    ensure!(
        gear_ratio.is_finite() && gear_ratio != 0.0,
        "motor gear ratio must be finite and nonzero"
    );
    let (mode, value) = match command {
        api::component::motor::Command::Position(value) => {
            (MotorCommand::Position, f64::from(*value))
        }
        api::component::motor::Command::Velocity(value) => {
            (MotorCommand::Velocity, f64::from(*value))
        }
        api::component::motor::Command::Torque(value) => (MotorCommand::Torque, f64::from(*value)),
        api::component::motor::Command::Stop => unreachable!(),
    };
    ensure!(
        mode == configured,
        "motor command mode does not match its plan"
    );
    let value = match mode {
        MotorCommand::Position | MotorCommand::Velocity => value / gear_ratio,
        MotorCommand::Torque => value * gear_ratio,
    };
    ensure!(
        value.is_finite(),
        "motor command becomes non-finite after gearing"
    );
    Ok(match mode {
        MotorCommand::Position => MotorAction::Position(value),
        MotorCommand::Velocity => MotorAction::Velocity(value),
        MotorCommand::Torque => MotorAction::Torque(value),
    })
}

pub(crate) fn ensure_motor_plan(binding: &PlannedBinding, command: MotorCommand) -> Result<()> {
    let planned = binding
        .motor_command()
        .context("motor binding has no command contract")?;
    ensure!(
        planned == command,
        "motor binding command mode does not match its plan"
    );
    Ok(())
}
