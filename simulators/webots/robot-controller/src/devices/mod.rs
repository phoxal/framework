use super::*;

pub(super) mod encoder;
pub(super) mod motor;

use encoder::EncoderDevice;
use motor::{
    MotorAction, MotorDevice, classify_selection, dispatch_motor, ensure_motor_plan, stop_every,
};

pub(super) struct DeviceSet {
    motors: Vec<MotorDevice>,
    encoders: Vec<EncoderDevice>,
    sensors: SensorSet,
}

impl DeviceSet {
    pub(super) async fn bind(
        session: &SimulatorSession,
        webots: &Webots,
        plan: &RobotSimulationPlan,
        source_start_ns: u64,
    ) -> Result<Self> {
        let mut motors = Vec::new();
        let mut encoders = Vec::new();
        let drive_authority = DriveCommandAuthority::standard()?;
        for binding in &plan.capabilities {
            if !matches!(
                binding.kind(),
                CapabilityKind::Motor | CapabilityKind::Encoder
            ) {
                continue;
            }
            let declared = session
                .robot()
                .capability(binding.reference())
                .with_context(|| format!("plan capability {} is absent", binding.reference()))?;
            let component = || api::topics().component(&binding.reference().component_id);
            let id = &binding.reference().capability_id;
            match (declared, binding.kind()) {
                (DeclaredCapability::Motor(config), CapabilityKind::Motor) => {
                    ensure_motor_plan(binding, config.command)?;
                    let native = webots.motor(binding.native_device())?;
                    let position_velocity = config.max_velocity_radps.map_or_else(
                        || native.get_max_velocity(),
                        |velocity| Ok(velocity / config.gear_ratio.abs()),
                    )?;
                    ensure!(
                        position_velocity.is_finite() && position_velocity > 0.0,
                        "motor {} has no positive finite position velocity",
                        binding.reference()
                    );
                    motors.push(MotorDevice {
                        capability: binding.reference().clone(),
                        native,
                        command: config.command,
                        gear_ratio: config.gear_ratio,
                        position_velocity,
                        receiver: session
                            .setpoint_receiver(component()?.motor(id)?.command().owner())
                            .await?,
                        authority: drive_authority.motor_lease(),
                        ready: session
                            .participant_ready_events(drive_authority.source())
                            .await?,
                    });
                }
                (DeclaredCapability::Encoder(config), CapabilityKind::Encoder) => {
                    let sampling = binding
                        .sampling()
                        .context("encoder plan has no sampling policy")?;
                    let native = webots.position_sensor(binding.native_device())?;
                    native.enable(sampling.native_period_ms)?;
                    encoders.push(EncoderDevice {
                        native,
                        gear_ratio: config.gear_ratio,
                        schedule: sensors::schedule(binding, source_start_ns)?,
                        last: None,
                        publisher: session
                            .sample_publisher(component()?.encoder(id)?.sample().owner())?,
                    });
                }
                _ => bail!(
                    "plan binding {} does not match its compiled capability kind",
                    binding.reference()
                ),
            }
        }
        let sensors = SensorSet::bind(session, webots, &plan.capabilities, source_start_ns)?;
        Ok(Self {
            motors,
            encoders,
            sensors,
        })
    }

    pub(super) fn prepare_transition(
        &mut self,
        boundary: &ActiveBoundaryStamp,
        selected_from: WorldProgress,
    ) -> Result<Vec<PendingActuationEvidence>> {
        let mut evidence = Vec::with_capacity(self.motors.len());
        for motor in &mut self.motors {
            if motor.receiver.terminal().is_some() {
                motor.apply_action(MotorAction::Stop)?;
                evidence.push(PendingActuationEvidence {
                    capability: motor.capability.clone(),
                    revision: boundary.revision(),
                    selected_at: boundary.instant(),
                    selected_from,
                    offered: Vec::new(),
                    selected: None,
                    selection: ActuationSelection::None {
                        reason: NoActuationReason::ReceiverClosed,
                    },
                    applied: AppliedActuation::Stop,
                });
                continue;
            }
            while let Some(event) = motor.ready.try_recv() {
                motor.authority.update_ready_event(&event);
            }
            if motor.ready.overflowed() {
                motor.authority.mark_ready_overflow();
            }
            let mut offered = Vec::new();
            while let Some(observed) = motor.receiver.try_recv_at(boundary) {
                let command = observed.body.clone();
                let decision = motor.authority.offer(
                    observed.metadata.source.participant_source(),
                    observed.metadata.sequence,
                    observed.observed_at,
                    observed.body,
                );
                offered.push(OfferedActuation {
                    producer: observed
                        .metadata
                        .source
                        .participant_source()
                        .map(|source| source.producer),
                    sequence: observed.metadata.sequence,
                    command,
                    decision: evidence_decision(decision),
                });
            }
            let held_before_selection = motor.authority.producer().is_some();
            let ready_count = motor.authority.ready_count();
            let accepted_new = offered.iter().any(|offered| {
                matches!(
                    offered.decision,
                    ActuationDecision::Acquired | ActuationDecision::Renewed
                )
            });
            let selected = motor.authority.live_host(boundary.local_instant()).cloned();
            let selection = classify_selection(
                selected.is_some(),
                accepted_new,
                held_before_selection,
                ready_count,
                !offered.is_empty(),
            );
            let action = selected
                .as_ref()
                .map(|command| dispatch_motor(motor.command, command, motor.gear_ratio))
                .transpose()?
                .unwrap_or(MotorAction::Stop);
            motor.apply_action(action)?;
            evidence.push(PendingActuationEvidence {
                capability: motor.capability.clone(),
                revision: boundary.revision(),
                selected_at: boundary.instant(),
                selected_from,
                offered,
                selected,
                selection,
                applied: action.into(),
            });
        }
        Ok(evidence)
    }

    pub(super) fn publish_outputs(&mut self, transition: &LiveTransitionStamp) -> Result<()> {
        self.sensors.publish_outputs(transition)?;
        for encoder in &mut self.encoders {
            encoder.publish_output(transition)?;
        }
        Ok(())
    }

    pub(super) fn invalidate_and_park(&mut self) -> Result<()> {
        for motor in &mut self.motors {
            motor.receiver.flush();
            motor.authority.clear();
        }
        self.stop_all_motors()
    }

    pub(super) fn stop_native(&mut self) -> Result<()> {
        self.stop_all_motors()
    }

    /// Attempt every independent native motor stop before reporting cleanup failure.
    fn stop_all_motors(&mut self) -> Result<()> {
        stop_every(
            &mut self.motors,
            |motor| motor.capability.to_string(),
            |motor| motor.stop(),
        )
    }
}
