//! `drive` - the official differential-drive participant.
//!
//! A scheduled participant that closes the body-twist to wheel-velocity loop.
//! It subscribes to `drive/target`, clamps it to the configured linear and
//! angular limits, mixes the limited twist into per-wheel angular speeds via
//! differential inverse kinematics, and commands each wheel motor on its dynamic
//! `component/<id>/motor/<cap>/command` topic.
//! It also publishes `drive/state` as one active/stopped decision carrying the
//! requested target and, when active, the limited target.
//! The per-side motor bindings and wheel geometry are built from the robot
//! model. Unsupported kinematics and motor command modes fail setup rather
//! than entering a runtime inactive state.
//! `drive/target` is internal actuation, not an observation: it carries no
//! production timestamp. Its liveness is therefore a **receiver-owned lease** -
//! `drive` stamps its own observation, rejects a non-increasing sequence from
//! the accepted producer, and applies the held command at a logical step only
//! while both expiry conditions hold: a host-monotonic silence deadline and a
//! logical hold horizon. Either one elapsing stops the wheels rather than
//! carrying the last command. On shutdown it makes a best-effort pass to park
//! every wheel before the bus closes.

use std::time::Duration;

use anyhow::{Result, bail};
use phoxal::api;
use phoxal::model::Robot;
use phoxal::model::component::capability::MotorCommand;
use phoxal::model::identity::CapabilityRef;
use phoxal::model::robot::{BodyTwist, DifferentialDrive, KinematicConfig, MotionLimits};
use phoxal::prelude::*;

/// How long `drive` tolerates silence from the accepted target producer. This
/// is human/network liveness and runs on the host clock, so an accelerated
/// simulation does not stretch it.
const TARGET_SILENCE: Duration = Duration::from_millis(500);

/// How far the robot may travel on one held target. This is bounded travel and
/// runs on robot time, so a decelerated simulation does not shrink it.
const TARGET_HOLD: Duration = Duration::from_millis(500);

/// The commandable wheel speeds a drive target implies.
///
/// The geometry itself lives on [`DifferentialDrive`] in `phoxal-model`, beside
/// the kinematic config it is read from. What belongs here is the part that is
/// about *this* contract: narrowing a twist expressed in `api::drive::Target`
/// into something a motor command can carry.
trait DriveTargetKinematics {
    /// The `(left, right)` wheel speeds `target` asks for, or `None` when the
    /// geometry turns a finite twist into a command no motor can carry: a
    /// non-finite speed, or one that does not survive the narrowing to the
    /// `f32` a motor command is expressed in.
    fn wheel_targets(self, target: &api::drive::Target) -> Option<(f64, f64)>;
}

impl DriveTargetKinematics for DifferentialDrive {
    fn wheel_targets(self, target: &api::drive::Target) -> Option<(f64, f64)> {
        let speeds = self.wheel_speeds(BodyTwist::planar(
            f64::from(target.linear_x_mps()),
            f64::from(target.angular_z_radps()),
        ));
        let (left, right) = (speeds.left_radps, speeds.right_radps);
        (left.is_finite()
            && right.is_finite()
            && left.abs() <= f64::from(f32::MAX)
            && right.abs() <= f64::from(f32::MAX))
        .then_some((left, right))
    }
}

/// One actuator binding resolved from the robot model.
struct MotorBinding {
    reference: CapabilityRef,
    direction_sign: i8,
}

impl MotorBinding {
    fn resolve(robot: &Robot, references: &[CapabilityRef], field: &str) -> Result<Vec<Self>> {
        if references.is_empty() {
            bail!("robot.kinematic.{field} must list at least one actuator");
        }
        references
            .iter()
            .map(|reference| {
                let (motor, direction_sign) = robot.require_motor(reference)?;
                if motor.command != MotorCommand::Velocity {
                    bail!(
                        "robot.kinematic.{field} actuator '{reference}' uses {:?} command mode; stock drive requires velocity motors",
                        motor.command
                    );
                }
                Ok(MotorBinding {
                    reference: reference.clone(),
                    direction_sign,
                })
            })
            .collect()
    }

    /// The dynamic per-instance motor-command topic for this binding. Drive
    /// CLIENT-publishes motor commands (the motor driver owns/subscribes them), so
    /// this is the `Publish` side from the public builder.
    fn topic(
        &self,
    ) -> Result<
        phoxal::bus::Topic<phoxal::bus::Publish<api::endpoint::component::motor::CommandEndpoint>>,
    > {
        Ok(api::topic::client()
            .component(&self.reference.component_id)?
            .motor(&self.reference.capability_id)?
            .command())
    }

    /// `wheel_radps` as this motor's own command, turned the way the robot
    /// model says this actuator is mounted.
    fn command(&self, wheel_radps: f64) -> api::component::motor::Command {
        api::component::motor::Command::Velocity(
            (wheel_radps * f64::from(self.direction_sign)) as f32,
        )
    }
}

/// One wheel motor the service commands: its model binding and the publisher
/// that carries its commands.
///
/// The two are one record rather than two vectors read at the same index,
/// because a command paired with another motor's direction sign would turn a
/// wheel the wrong way with nothing to report.
/// The production sink is [`SetpointPublisher`]. An in-process bus cannot
/// deterministically fail only one publisher: all cloned publishers share one
/// private `BusHandle` outbound queue, so saturation or close fails every motor
/// handle together. Keeping this narrow send seam in production lets the
/// `Drive` command and shutdown paths be exercised with a deterministic sink
/// while preserving the exact publisher call and error contract.
trait MotorCommandSink {
    fn send(&self, command: api::component::motor::Command) -> Result<()>;
}

impl MotorCommandSink for SetpointPublisher<api::endpoint::component::motor::CommandEndpoint> {
    fn send(&self, command: api::component::motor::Command) -> Result<()> {
        Ok(SetpointPublisher::send(self, command)?)
    }
}

struct BoundMotor<P = SetpointPublisher<api::endpoint::component::motor::CommandEndpoint>> {
    binding: MotorBinding,
    publisher: P,
}

/// The operation being fanned out to the configured actuators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FanoutOperation {
    Command,
    Stop,
}

impl std::fmt::Display for FanoutOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Command => "command",
            Self::Stop => "stop",
        })
    }
}

/// One actuator failure retained while the remaining fanout is attempted.
#[derive(Debug)]
struct FanoutFailure {
    reference: CapabilityRef,
    operation: FanoutOperation,
    error: anyhow::Error,
}

/// Aggregate failure for one actuator fanout.
#[derive(Debug)]
struct FanoutError {
    failures: Vec<FanoutFailure>,
}

impl std::fmt::Display for FanoutError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} actuator fanout failure(s): ",
            self.failures.len()
        )?;
        for (index, failure) in self.failures.iter().enumerate() {
            if index != 0 {
                formatter.write_str(", ")?;
            }
            write!(
                formatter,
                "{} {} ({})",
                failure.operation, failure.reference, failure.error
            )?;
        }
        Ok(())
    }
}

trait FanoutTarget {
    fn capability_ref(&self) -> &CapabilityRef;
}

impl<P> FanoutTarget for BoundMotor<P> {
    fn capability_ref(&self) -> &CapabilityRef {
        &self.binding.reference
    }
}

/// Attempt every target and retain the exact target and operation for each
/// failure. The caller gets one aggregate only after the full pass completes.
fn fanout<'a, T, I, F>(
    targets: I,
    operation: FanoutOperation,
    mut operation_fn: F,
) -> std::result::Result<(), FanoutError>
where
    T: FanoutTarget + 'a,
    I: IntoIterator<Item = &'a T>,
    F: FnMut(&T) -> Result<()>,
{
    let mut failures = Vec::new();
    for target in targets {
        if let Err(error) = operation_fn(target) {
            failures.push(FanoutFailure {
                reference: target.capability_ref().clone(),
                operation,
                error,
            });
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(FanoutError { failures })
    }
}

impl std::error::Error for FanoutError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.failures
            .first()
            .map(|failure| failure.error.as_ref() as &(dyn std::error::Error + 'static))
    }
}

fn combine_fanouts(
    results: impl IntoIterator<Item = std::result::Result<(), FanoutError>>,
) -> Result<()> {
    let failures = results
        .into_iter()
        .filter_map(std::result::Result::err)
        .flat_map(|error| error.failures)
        .collect::<Vec<_>>();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::Error::new(FanoutError { failures }))
    }
}

/// The exact command orchestration used by [`Drive::step`]. Both sides are
/// evaluated before their failures are combined, and the injected operation
/// closures keep this production path directly testable without a live bus.
fn command_fanout<'a, T, LI, RI, LF, RF>(
    left: LI,
    right: RI,
    left_operation: LF,
    right_operation: RF,
) -> Result<()>
where
    T: FanoutTarget + 'a,
    LI: IntoIterator<Item = &'a T>,
    RI: IntoIterator<Item = &'a T>,
    LF: FnMut(&T) -> Result<()>,
    RF: FnMut(&T) -> Result<()>,
{
    combine_fanouts([
        fanout(left, FanoutOperation::Command, left_operation),
        fanout(right, FanoutOperation::Command, right_operation),
    ])
}

/// The command-and-state ordering used by [`Drive::step`]. State is published
/// only after every actuator command has been attempted successfully.
fn command_then_publish<'a, T, LI, RI, LF, RF, PF>(
    left: LI,
    right: RI,
    left_operation: LF,
    right_operation: RF,
    publish: PF,
) -> Result<()>
where
    T: FanoutTarget + 'a,
    LI: IntoIterator<Item = &'a T>,
    RI: IntoIterator<Item = &'a T>,
    LF: FnMut(&T) -> Result<()>,
    RF: FnMut(&T) -> Result<()>,
    PF: FnOnce() -> Result<()>,
{
    command_fanout(left, right, left_operation, right_operation)?;
    publish()
}

/// The exact stop orchestration used by [`Drive::shutdown`].
fn stop_fanout<'a, T, I, F>(targets: I, operation: F) -> Result<()>
where
    T: FanoutTarget + 'a,
    I: IntoIterator<Item = &'a T>,
    F: FnMut(&T) -> Result<()>,
{
    fanout(targets, FanoutOperation::Stop, operation).map_err(anyhow::Error::new)
}

impl<P: MotorCommandSink> BoundMotor<P> {
    /// Command this wheel at `wheel_radps`.
    fn drive(&self, wheel_radps: f64) -> Result<()> {
        self.publisher.send(self.binding.command(wheel_radps))
    }

    /// Stop this wheel, preserving the same failure path as a live command.
    fn stop(&self) -> Result<()> {
        self.publisher.send(api::component::motor::Command::Stop)
    }
}

/// Typed drive config built from the robot model.
struct DriveConfig {
    kinematics: DifferentialDrive,
    limits: MotionLimits,
    left: Vec<MotorBinding>,
    right: Vec<MotorBinding>,
}

impl DriveConfig {
    fn from_robot(robot: &Robot) -> Result<Self> {
        let limits = robot.motion().limits().validate()?;
        let KinematicConfig::Differential {
            left_actuators,
            right_actuators,
            wheel_radius_m,
            wheel_base_m,
            ..
        } = robot.motion().kinematic()
        else {
            bail!(
                "stock drive requires differential kinematics, found {:?}",
                robot.motion().kinematic().kind()
            );
        };
        Ok(DriveConfig {
            kinematics: DifferentialDrive::new(*wheel_radius_m, *wheel_base_m).validate()?,
            limits,
            left: MotorBinding::resolve(robot, left_actuators, "left_actuators")?,
            right: MotorBinding::resolve(robot, right_actuators, "right_actuators")?,
        })
    }
}

pub(crate) struct Api {
    target: SetpointReceiver<api::endpoint::drive::TargetEndpoint>,
    ready: phoxal::bus::ParticipantReadyEvents,
    state: StatePublisher<api::endpoint::drive::StateEndpoint>,
    left_motors: Vec<BoundMotor>,
    right_motors: Vec<BoundMotor>,
}

pub(crate) struct DriveState {
    /// Validated differential-drive geometry. Unsupported topology is rejected
    /// by setup before the participant can enter its step loop.
    kinematics: DifferentialDrive,
    limits: MotionLimits,
    target: FixedSourceLease<api::drive::Target>,
}

impl DriveState {
    /// The state to publish and the (left, right) wheel speeds to command for
    /// this step.
    ///
    /// The lease is the only source of a live target: it yields nothing when
    /// nothing has been accepted, when the producer has gone silent past the
    /// host deadline, or when the held command has been applied for longer than
    /// the logical hold horizon. All three stop the wheels rather than carrying
    /// the last command.
    fn decide(
        &mut self,
        host_now: LocalInstant,
        now: RobotInstant,
    ) -> (api::drive::State, (f64, f64)) {
        let stopped = |target, reason| (api::drive::State::Stopped { target, reason }, (0.0, 0.0));

        let limits = self.limits;
        let kinematics = self.kinematics;
        let Some(target) = self.target.live(host_now, now).cloned() else {
            return stopped(
                api::drive::Target::stopped(),
                api::drive::StopReason::TargetStale,
            );
        };
        // A target decoded from the bus or built by a caller is finite by
        // construction. Keep this branch as a defensive check for values
        // produced inside this crate before the final wheel mix.
        if !(target.linear_x_mps().is_finite() && target.angular_z_radps().is_finite()) {
            return stopped(target, api::drive::StopReason::TargetNotFinite);
        }

        let clamp = |value: f32, limit: f64| value.clamp(-limit as f32, limit as f32);
        let Ok(limited_target) = api::drive::Target::try_new(
            clamp(target.linear_x_mps(), limits.max_linear_speed_mps),
            clamp(target.angular_z_radps(), limits.max_angular_speed_radps),
        ) else {
            return stopped(target, api::drive::StopReason::TargetNotFinite);
        };
        let Some(wheels) = kinematics.wheel_targets(&limited_target) else {
            return stopped(target, api::drive::StopReason::ActuatorCommandNotFinite);
        };
        (
            api::drive::State::Active {
                target,
                limited_target,
            },
            wheels,
        )
    }
}

#[phoxal::service(state = DriveState, api = Api)]
pub(crate) struct Drive;

impl Participant for Drive {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let config = DriveConfig::from_robot(ctx.robot()?)?;
        let motion = phoxal::bus::ParticipantId::new("motion")
            .map_err(|error| anyhow::anyhow!("invalid fixed motion participant id: {error}"))?;
        let ready = ctx.participant_ready_events_for(&motion).await?;

        // Drive OWNS the `drive` node: it reads its command input and publishes its
        // telemetry through the owner builder.
        let target = ctx
            .setpoint_receiver(api::topic::owner().drive().target())
            .await?;
        let state = ctx.state_publisher(api::topic::owner().drive().state())?;

        let mut left_motors = Vec::with_capacity(config.left.len());
        for binding in config.left {
            let publisher = ctx.setpoint_publisher(binding.topic()?)?;
            left_motors.push(BoundMotor { binding, publisher });
        }
        let mut right_motors = Vec::with_capacity(config.right.len());
        for binding in config.right {
            let publisher = ctx.setpoint_publisher(binding.topic()?)?;
            right_motors.push(BoundMotor { binding, publisher });
        }

        Ok((
            DriveState {
                kinematics: config.kinematics,
                limits: config.limits,
                target: FixedSourceLease::new("drive/target", motion, TARGET_SILENCE, TARGET_HOLD),
            },
            Api {
                target,
                ready,
                state,
                left_motors,
                right_motors,
            },
        ))
    }

    fn reset(&self, _ctx: ResetContext, _api: &Self::Api, state: &mut Self::State) -> Result<()> {
        state.target.clear();
        Ok(())
    }

    #[phoxal::step(hz = 50)]
    fn step(&self, api: &Self::Api, step: StepContext, state: &mut Self::State) -> Result<()> {
        let now = step.now();
        // Without the host clock there is no silence deadline to measure, so
        // this step decides nothing: it renews no lease and applies no
        // command, and the leases expire on their own. The runner's own clock
        // read faults the participant on the same step.
        let Some(host_now) = LocalInstant::try_now() else {
            bail!("the host boot clock could not be read");
        };

        while let Some(event) = api.ready.try_recv() {
            state.target.update_ready_event(&event);
        }
        if api.ready.overflowed() {
            state.target.mark_ready_overflow();
        }

        // Offer every inbound target to the fixed-source lease. Packet arrival
        // cannot transfer authority: the exact Ready source set decides.
        while let Some(observed) = api.target.try_recv() {
            let decision = state.target.offer(
                observed.metadata.source.participant_source(),
                observed.metadata.sequence,
                observed.observed_at,
                observed.body,
            );
            if let LeaseDecision::Rejected(rejection) = decision {
                tracing::warn!(target: "phoxal.drive", error = %rejection, "rejected drive target");
            }
        }

        let (published, (left, right)) = state.decide(host_now, now);
        // Evaluate both side fanouts before combining their errors: a failed
        // left motor must never prevent a right-side command from being tried.
        command_then_publish(
            &api.left_motors,
            &api.right_motors,
            |motor| motor.drive(left),
            |motor| motor.drive(right),
            || Ok(api.state.publish(&step.token, published)?),
        )?;
        Ok(())
    }

    async fn shutdown(&self, api: &Self::Api, _state: &mut Self::State) -> Result<()> {
        stop_fanout(api.left_motors.iter().chain(&api.right_motors), |motor| {
            motor.stop()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use phoxal::api;
    use phoxal::bus::{
        FixedSourceLease, LeaseDecision, LocalInstant, ParticipantId, ParticipantReadyStatus,
        ParticipantSourceIdentity, ProducerId, RobotInstant, TimelineId,
    };
    use phoxal::model::RobotBuilder;
    use phoxal::model::builder::Kinematics;

    use super::{
        BoundMotor, DifferentialDrive, DriveConfig, DriveState, DriveTargetKinematics, Duration,
        FanoutOperation, FanoutTarget, MotionLimits, MotorBinding, MotorCommandSink, TARGET_HOLD,
        TARGET_SILENCE, command_fanout, command_then_publish, fanout, stop_fanout,
    };

    #[derive(Debug)]
    struct InjectedError(&'static str);

    impl std::fmt::Display for InjectedError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(self.0)
        }
    }

    impl std::error::Error for InjectedError {}

    struct TestSink {
        attempts: std::rc::Rc<RefCell<Vec<api::component::motor::Command>>>,
        fails: bool,
    }

    impl MotorCommandSink for TestSink {
        fn send(&self, command: api::component::motor::Command) -> anyhow::Result<()> {
            self.attempts.borrow_mut().push(command);
            if self.fails {
                Err(anyhow::Error::new(InjectedError(
                    "injected publisher failure",
                )))
            } else {
                Ok(())
            }
        }
    }

    struct TestTarget {
        reference: super::CapabilityRef,
    }

    impl FanoutTarget for TestTarget {
        fn capability_ref(&self) -> &super::CapabilityRef {
            &self.reference
        }
    }

    fn targets(names: &[&str]) -> Vec<TestTarget> {
        names
            .iter()
            .map(|name| TestTarget {
                reference: name.parse().expect("a normalized capability reference"),
            })
            .collect()
    }

    fn bound_test_motors(
        names: &[&str],
        failing_index: usize,
    ) -> (
        Vec<BoundMotor<TestSink>>,
        std::rc::Rc<RefCell<Vec<api::component::motor::Command>>>,
    ) {
        let attempts = std::rc::Rc::new(RefCell::new(Vec::new()));
        let motors = names
            .iter()
            .enumerate()
            .map(|(index, name)| BoundMotor {
                binding: MotorBinding {
                    reference: name.parse().expect("a normalized capability reference"),
                    direction_sign: 1,
                },
                publisher: TestSink {
                    attempts: attempts.clone(),
                    fails: index == failing_index,
                },
            })
            .collect();
        (motors, attempts)
    }

    /// A distinct deterministic test producer. Production sessions mint their
    /// producer through the bus owner, while tests name theirs explicitly.
    fn producer(value: u128) -> ProducerId {
        ProducerId::try_from((1_u128 << 124) | value).expect("a test producer is canonical")
    }

    #[test]
    fn production_command_fanout_attempts_every_target_after_first_failure() {
        let targets = targets(&["left_front.motor", "left_rear.motor", "right_front.motor"]);
        let invoked = RefCell::new(Vec::new());

        let error = command_fanout(
            &targets[..1],
            &targets[1..],
            |target| {
                invoked.borrow_mut().push(target.reference.to_string());
                Err(anyhow::Error::new(InjectedError(
                    "injected first-target failure",
                )))
            },
            |target| {
                invoked.borrow_mut().push(target.reference.to_string());
                Ok(())
            },
        )
        .expect_err("the injected first failure must be returned");
        let aggregate = error
            .downcast_ref::<super::FanoutError>()
            .expect("production command orchestration returns the aggregate");

        assert_eq!(
            invoked.into_inner(),
            ["left_front.motor", "left_rear.motor", "right_front.motor"]
        );
        assert_eq!(aggregate.failures.len(), 1);
        assert_eq!(
            aggregate.failures[0].reference.to_string(),
            "left_front.motor"
        );
        assert_eq!(aggregate.failures[0].operation, FanoutOperation::Command);
        assert!(
            aggregate.failures[0]
                .error
                .downcast_ref::<InjectedError>()
                .is_some()
        );
        assert!(std::error::Error::source(aggregate).is_some());
    }

    #[test]
    fn production_bound_motors_attempt_all_commands_and_skip_state_after_failure() {
        let (motors, attempts) = bound_test_motors(
            &["left_front.motor", "left_rear.motor", "right_front.motor"],
            0,
        );
        let published = std::cell::Cell::new(false);

        let error = command_then_publish(
            &motors[..1],
            &motors[1..],
            |motor| motor.drive(1.0),
            |motor| motor.drive(1.0),
            || {
                published.set(true);
                Ok(())
            },
        )
        .expect_err("a failing bound motor must fault the command step");
        let aggregate = error
            .downcast_ref::<super::FanoutError>()
            .expect("the production step seam returns the aggregate");

        assert_eq!(attempts.borrow().len(), 3);
        assert!(!published.get(), "state must follow successful fanout only");
        assert_eq!(aggregate.failures.len(), 1);
        assert_eq!(
            aggregate.failures[0].reference.to_string(),
            "left_front.motor"
        );
        assert!(
            aggregate.failures[0]
                .error
                .downcast_ref::<InjectedError>()
                .is_some()
        );
    }

    #[test]
    fn production_bound_motors_attempt_all_shutdown_stops_and_propagate() {
        let (motors, attempts) = bound_test_motors(
            &["left_front.motor", "left_rear.motor", "right_front.motor"],
            0,
        );

        let error = stop_fanout(motors.iter(), |motor| motor.stop())
            .expect_err("a failing stop must be retained by shutdown");
        let aggregate = error
            .downcast_ref::<super::FanoutError>()
            .expect("shutdown seam returns the aggregate");

        assert_eq!(attempts.borrow().len(), 3);
        assert_eq!(aggregate.failures.len(), 1);
        assert_eq!(aggregate.failures[0].operation, FanoutOperation::Stop);
        assert!(error.to_string().contains("left_front.motor"));
    }

    #[test]
    fn actuator_fanout_retains_every_failure_reference_and_operation() {
        let targets = targets(&["left_front.motor", "left_rear.motor", "right_front.motor"]);

        let error = fanout(&targets, FanoutOperation::Command, |target| {
            if target.reference.to_string() != "left_rear.motor" {
                return Err(anyhow::Error::new(InjectedError("injected failure")));
            }
            Ok(())
        })
        .expect_err("two injected failures must be aggregated");

        assert_eq!(
            error
                .failures
                .iter()
                .map(|failure| failure.reference.to_string())
                .collect::<Vec<_>>(),
            ["left_front.motor", "right_front.motor"]
        );
        assert!(
            error
                .failures
                .iter()
                .all(|failure| failure.operation == FanoutOperation::Command)
        );
        assert!(
            error
                .failures
                .iter()
                .all(|failure| failure.error.downcast_ref::<InjectedError>().is_some())
        );
        let display = error.to_string();
        assert!(display.contains("left_front.motor"));
        assert!(display.contains("right_front.motor"));
        assert!(display.contains("command"));
    }

    #[test]
    fn stop_fanout_attempts_all_targets_and_propagates_failures() {
        let targets = targets(&["left_front.motor", "right_front.motor"]);
        let mut invoked = Vec::new();

        let error = stop_fanout(&targets, |target| {
            invoked.push(target.reference.to_string());
            Err(anyhow::Error::new(InjectedError("injected stop failure")))
        })
        .expect_err("stop failures must be propagated");
        let aggregate = error
            .downcast_ref::<super::FanoutError>()
            .expect("production shutdown orchestration returns the aggregate");

        assert_eq!(invoked, ["left_front.motor", "right_front.motor"]);
        assert_eq!(aggregate.failures.len(), 2);
        assert!(
            aggregate
                .failures
                .iter()
                .all(|failure| failure.operation == FanoutOperation::Stop)
        );
        assert!(
            aggregate
                .failures
                .iter()
                .all(|failure| failure.error.downcast_ref::<InjectedError>().is_some())
        );
        assert!(error.to_string().contains("stop"));
    }

    const LIMITS: MotionLimits = MotionLimits {
        max_linear_speed_mps: 0.6,
        max_angular_speed_radps: 2.0,
    };

    const KINEMATICS: DifferentialDrive = DifferentialDrive::new(0.1, 0.4);

    fn drive_state(kinematics: DifferentialDrive) -> DriveState {
        let motion = ParticipantId::new("motion").unwrap();
        let source = producer(1);
        let mut target =
            FixedSourceLease::new("drive/target", motion.clone(), TARGET_SILENCE, TARGET_HOLD);
        target.update_ready(
            &ParticipantSourceIdentity::new(motion.clone(), source),
            ParticipantReadyStatus::Ready,
        );
        DriveState {
            kinematics,
            limits: LIMITS,
            target,
        }
    }

    /// The instants a single-step decision is taken at.
    fn instants() -> (LocalInstant, RobotInstant) {
        (
            LocalInstant::from_boot_ns(0),
            RobotInstant::new(TimelineId::mint(), 0),
        )
    }

    #[test]
    fn wheel_command_overflow_fails_closed() {
        let kinematics = DifferentialDrive::new(f64::MIN_POSITIVE, 0.4);
        let target = api::drive::Target::try_new(0.6, 0.0).unwrap();
        assert!(kinematics.wheel_targets(&target).is_none());
    }

    #[test]
    fn config_from_robot_resolves_per_side_motors() {
        // A 4-wheel differential: 2 motors per side, and the per-side lists are
        // what the config has to split on.
        let robot = RobotBuilder::new("rover")
            .component_type("drive_motor", |motor| motor.motor("motor", "motor_joint"))
            .component("front_left_drive", "drive_motor")
            .component("front_right_drive", "drive_motor")
            .component("rear_left_drive", "drive_motor")
            .component("rear_right_drive", "drive_motor")
            .kinematics(Kinematics::Differential {
                left_actuators: &["front_left_drive.motor", "rear_left_drive.motor"],
                right_actuators: &["front_right_drive.motor", "rear_right_drive.motor"],
                left_encoders: &[],
                right_encoders: &[],
                wheel_radius_m: 0.1,
                wheel_base_m: 0.4,
            })
            .build()
            .expect("a valid robot");

        let config = DriveConfig::from_robot(&robot).unwrap();

        assert_eq!(config.left.len(), 2);
        assert_eq!(config.right.len(), 2);
        assert_eq!(config.kinematics.wheel_radius_m, 0.1);
        // Each binding resolves to a concrete dynamic motor topic.
        let topic = config.left[0]
            .topic()
            .expect("compiled motor bindings are valid key segments");
        assert!(topic.key().starts_with("robot/component/"));
        assert!(topic.key().ends_with("/command"));
    }

    #[test]
    fn a_bound_motor_turns_the_way_the_model_mounts_it() {
        let binding = |direction_sign| super::MotorBinding {
            reference: "front_left_drive.spin"
                .parse()
                .expect("a normalized reference"),
            direction_sign,
        };
        let forward = binding(1);
        let reversed = binding(-1);
        assert_eq!(
            forward.command(2.5),
            api::component::motor::Command::Velocity(2.5)
        );
        assert_eq!(
            reversed.command(2.5),
            api::component::motor::Command::Velocity(-2.5)
        );
    }

    #[test]
    fn a_decision_reports_raw_requested_and_limited_targets() {
        let requested = api::drive::Target::try_new(5.0, -5.0).unwrap();
        let (host_now, now) = instants();
        let mut state = drive_state(KINEMATICS);
        let motion = state.target.expected_participant().clone();
        state.target.offer(
            Some(&ParticipantSourceIdentity::new(motion.clone(), producer(1))),
            1,
            host_now,
            requested.clone(),
        );

        let (published, wheels) = state.decide(host_now, now);

        let api::drive::State::Active {
            target,
            limited_target,
        } = published
        else {
            panic!("a live finite target must produce an active drive state");
        };
        assert_eq!(target, requested);
        assert_eq!(limited_target.linear_x_mps(), 0.6);
        assert_eq!(limited_target.angular_z_radps(), -2.0);
        // The wheels are driven from the *limited* target, not the raw request.
        let (left, right) = wheels;
        assert!((left - 10.0).abs() < 1e-6 && (right - 2.0).abs() < 1e-6);
    }

    #[test]
    fn nothing_live_stops_the_wheels_rather_than_carrying_the_last_command() {
        let (host_now, now) = instants();
        let (published, wheels) = drive_state(KINEMATICS).decide(host_now, now);

        assert!(matches!(
            published,
            api::drive::State::Stopped {
                target,
                reason: api::drive::StopReason::TargetStale,
            } if target == api::drive::Target::stopped()
        ));
        assert_eq!(wheels, (0.0, 0.0));
    }

    /// A robot the service has no supported kinematics for fails setup before
    /// the participant can enter its step loop.
    #[test]
    fn unsupported_kinematics_fail_setup_instead_of_entering_an_inactive_state() {
        let robot = RobotBuilder::new("rover").build().expect("minimal robot");
        assert!(DriveConfig::from_robot(&robot).is_err());
    }

    /// A target the wheels cannot carry stops them, and still reports the raw
    /// request so a consumer can see what was asked for.
    #[test]
    fn a_wheel_command_the_motors_cannot_carry_stops_the_wheels() {
        let requested = api::drive::Target::try_new(0.6, 0.0).unwrap();
        let (host_now, now) = instants();
        let mut state = drive_state(DifferentialDrive::new(f64::MIN_POSITIVE, 0.4));
        let motion = state.target.expected_participant().clone();
        state.target.offer(
            Some(&ParticipantSourceIdentity::new(motion.clone(), producer(1))),
            1,
            host_now,
            requested.clone(),
        );

        let (published, wheels) = state.decide(host_now, now);

        assert!(matches!(
            published,
            api::drive::State::Stopped {
                target,
                reason: api::drive::StopReason::ActuatorCommandNotFinite,
            } if target == requested
        ));
        assert_eq!(wheels, (0.0, 0.0));
    }

    /// Either expiry condition alone stops the wheels. Host silence is checked
    /// on the host clock so an accelerated simulation cannot stretch it, and the
    /// hold horizon is checked on robot time so a decelerated one cannot shrink
    /// it.
    #[test]
    fn either_lease_expiry_condition_alone_stops_the_wheels() {
        let requested = api::drive::Target::try_new(0.4, 0.0).unwrap();
        let producer = producer(1);
        let line = TimelineId::mint();
        let host_start = LocalInstant::from_boot_ns(0);
        let robot_start = RobotInstant::new(line, 0);

        let motion = ParticipantId::new("motion").unwrap();
        let mut silent =
            FixedSourceLease::new("drive/target", motion.clone(), TARGET_SILENCE, TARGET_HOLD);
        silent.update_ready(
            &ParticipantSourceIdentity::new(motion.clone(), producer),
            ParticipantReadyStatus::Ready,
        );
        silent.offer(
            Some(&ParticipantSourceIdentity::new(motion.clone(), producer)),
            1,
            host_start,
            requested.clone(),
        );
        assert!(silent.live(host_start, robot_start).is_some());
        let past_silence = host_start.saturating_add(TARGET_SILENCE + Duration::from_millis(1));
        assert!(
            silent.live(past_silence, robot_start).is_none(),
            "host silence expires the lease even while robot time stands still"
        );

        let mut held =
            FixedSourceLease::new("drive/target", motion.clone(), TARGET_SILENCE, TARGET_HOLD);
        held.update_ready(
            &ParticipantSourceIdentity::new(motion.clone(), producer),
            ParticipantReadyStatus::Ready,
        );
        held.offer(
            Some(&ParticipantSourceIdentity::new(motion.clone(), producer)),
            1,
            host_start,
            requested,
        );
        assert!(held.live(host_start, robot_start).is_some());
        let past_hold = robot_start.saturating_add(TARGET_HOLD + Duration::from_millis(1));
        assert!(
            held.live(host_start, past_hold).is_none(),
            "the logical horizon expires the lease even while host time stands still"
        );
    }

    /// A restarted publisher takes over only after the old Ready token is
    /// gone; packet arrival alone cannot perform the handoff.
    #[test]
    fn a_replacement_producer_takes_over_and_fences_the_previous_one() {
        let target = |linear_x_mps| api::drive::Target::try_new(linear_x_mps, 0.0).unwrap();
        let first = producer(2);
        let second = producer(3);
        let host_now = LocalInstant::from_boot_ns(0);
        let now = RobotInstant::new(TimelineId::mint(), 0);
        let motion = ParticipantId::new("motion").unwrap();
        let mut lease =
            FixedSourceLease::new("drive/target", motion.clone(), TARGET_SILENCE, TARGET_HOLD);
        lease.update_ready(
            &ParticipantSourceIdentity::new(motion.clone(), first),
            ParticipantReadyStatus::Ready,
        );
        lease.offer(
            Some(&ParticipantSourceIdentity::new(motion.clone(), first)),
            9,
            host_now,
            target(0.1),
        );
        lease.update_ready(
            &ParticipantSourceIdentity::new(motion.clone(), second),
            ParticipantReadyStatus::Ready,
        );
        assert!(matches!(
            lease.offer(
                Some(&ParticipantSourceIdentity::new(motion.clone(), second)),
                0,
                host_now,
                target(0.2),
            ),
            LeaseDecision::Rejected(_)
        ));
        lease.update_ready(
            &ParticipantSourceIdentity::new(motion.clone(), first),
            ParticipantReadyStatus::Lost,
        );
        assert!(matches!(
            lease.offer(
                Some(&ParticipantSourceIdentity::new(motion.clone(), second)),
                0,
                host_now,
                target(0.2),
            ),
            LeaseDecision::Acquired
        ));
        assert_eq!(lease.producer(), Some(second));
        assert_eq!(
            lease
                .live(host_now, now)
                .map(api::drive::Target::linear_x_mps),
            Some(0.2)
        );
        assert!(matches!(
            lease.offer(
                Some(&ParticipantSourceIdentity::new(motion.clone(), second)),
                0,
                host_now,
                target(0.3),
            ),
            LeaseDecision::Rejected(_)
        ));
    }
}
