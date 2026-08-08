//! One capability's Webots device bound to the bus handle that serves it.
//!
//! Binding the device and its handle into one record is what keeps them from
//! being re-paired by position on every step: a produced sample already
//! carries the publisher it belongs to, and a queued command already sits next
//! to the device it drives. The controller holds one `Vec<CapabilityChannel>`
//! in the robot's canonical capability order, so there is exactly one sequence
//! to keep straight instead of sixteen parallel ones.

use anyhow::Result;
use phoxal::api;
use phoxal::bus::{
    CaptureStamp, ContractBody, FixedSourceLease, LeaseDecision, LocalInstant, MeasurementContract,
    ParticipantId, ParticipantReadyEvents, StepStamp, WorldStepToken,
};
use phoxal::model::identity::CapabilityRef;
use phoxal::prelude::*;
use std::time::Duration;

use crate::capabilities::accelerometer::NativeAccelerometer;
use crate::capabilities::battery::NativeBattery;
use crate::capabilities::camera::NativeCamera;
use crate::capabilities::depth::NativeDepth;
use crate::capabilities::encoder::NativeEncoder;
use crate::capabilities::gnss::NativeGnss;
use crate::capabilities::gyroscope::NativeGyroscope;
use crate::capabilities::imu::NativeImu;
use crate::capabilities::led::NativeLed;
use crate::capabilities::lidar::NativeLidar;
use crate::capabilities::magnetometer::NativeMagnetometer;
use crate::capabilities::microphone::NativeMicrophone;
use crate::capabilities::mmwave::NativeMmwave;
use crate::capabilities::motor::NativeMotor;
use crate::capabilities::range::NativeRange;
use crate::capabilities::speaker::NativeSpeaker;
use crate::capabilities::{SensorStep, SimulatedSensor};
use crate::catalog::CapabilitySpec;
use crate::controller::WebotsControllerSimulator;

const MOTOR_SOURCE_SILENCE: Duration = Duration::from_millis(150);

/// Reading a subscriber's backlog the way one world step needs it.
///
/// Two answers are meaningful, and which one a capability wants is part of
/// what that capability is. A superseded setpoint has no effect worth
/// applying, so an actuator takes only the newest. A stream's chunks are not
/// alternatives - dropping one corrupts the sound rather than shortening it -
/// so a speaker takes every one in order.
trait CommandBacklog<B> {
    /// The newest queued value; everything it supersedes is dropped.
    fn take_newest(&self) -> Option<B>;

    /// Everything queued, oldest first.
    fn take_all(&self) -> Vec<B>;

    /// The newest body together with the trusted transport provenance that
    /// decides whether an actuator may apply it.
    fn take_newest_observed(&self) -> Option<Observed<B>>;
}

impl<B: ContractBody> CommandBacklog<B> for Subscriber<B> {
    fn take_newest(&self) -> Option<B> {
        let mut newest = None;
        while let Some(received) = self.try_recv() {
            newest = Some(received.body);
        }
        newest
    }

    fn take_all(&self) -> Vec<B> {
        let mut received = Vec::new();
        while let Some(message) = self.try_recv() {
            received.push(message.body);
        }
        received
    }

    fn take_newest_observed(&self) -> Option<Observed<B>> {
        let mut newest = None;
        while let Some(received) = self.try_recv() {
            newest = Some(received);
        }
        newest
    }
}

/// A Webots device the graph drives.
trait SimulatedActuator {
    /// The contract body this device is commanded with.
    type Command: ContractBody + Clone;

    /// Apply everything the graph sent since the previous step.
    fn apply_backlog(&mut self, commands: &Subscriber<Self::Command>) -> Result<()>;

    /// Apply one already-admitted command body.
    fn apply_body(&mut self, command: Self::Command) -> Result<()>;

    /// Leave the device quiet: a simulation that stopped must not keep driving
    /// or keep playing. A device with nothing to quiet keeps the default.
    fn park(&mut self) -> Result<()> {
        Ok(())
    }
}

impl SimulatedActuator for NativeMotor {
    type Command = api::component::motor::Command;

    fn apply_backlog(&mut self, commands: &Subscriber<Self::Command>) -> Result<()> {
        match commands.take_newest() {
            Some(command) => self.apply(&command),
            None => Ok(()),
        }
    }

    fn apply_body(&mut self, command: Self::Command) -> Result<()> {
        self.apply(&command)
    }

    fn park(&mut self) -> Result<()> {
        self.apply(&api::component::motor::Command::Stop)
    }
}

impl SimulatedActuator for NativeLed {
    type Command = api::component::led::Command;

    fn apply_backlog(&mut self, commands: &Subscriber<Self::Command>) -> Result<()> {
        match commands.take_newest() {
            Some(command) => self.apply(&command),
            None => Ok(()),
        }
    }

    fn apply_body(&mut self, command: Self::Command) -> Result<()> {
        self.apply(&command)
    }
}

impl SimulatedActuator for NativeSpeaker {
    type Command = api::component::speaker::Chunk;

    fn apply_backlog(&mut self, commands: &Subscriber<Self::Command>) -> Result<()> {
        // Audio chunks move rather than copy: a stream is large, and nothing
        // downstream needs them again.
        for chunk in commands.take_all() {
            self.apply(chunk)?;
        }
        Ok(())
    }

    fn apply_body(&mut self, command: Self::Command) -> Result<()> {
        self.apply(command)
    }

    fn park(&mut self) -> Result<()> {
        self.stop()
    }
}

/// A sensor device and the measurement handle it publishes on.
struct SensorChannel<S: SimulatedSensor>
where
    S::Sample: MeasurementContract,
{
    device: S,
    publisher: MeasurementPublisher<S::Sample>,
}

impl<S: SimulatedSensor> SensorChannel<S>
where
    S::Sample: MeasurementContract,
{
    fn reset(&mut self, logical_time_ns: u64) -> Result<()> {
        self.device.reset(logical_time_ns)
    }

    /// Read this step's sample, if there is one, and queue it for publishing
    /// on this channel's own handle.
    ///
    /// `wrap` is the family's [`PendingPublish`] constructor. The queue is one
    /// sequence over every family, so a queued body has to say which contract
    /// it belongs to; it does that by construction rather than by a lookup.
    fn read_into(
        &mut self,
        step: SensorStep,
        wrap: fn(MeasurementPublisher<S::Sample>, S::Sample) -> PendingPublish,
        pending: &mut Vec<PendingPublish>,
    ) -> Result<()> {
        if let Some(sample) = self.device.read_if_due(step)? {
            pending.push(wrap(self.publisher.clone(), sample));
        }
        Ok(())
    }
}

/// An actuator device and the subscriber carrying what the graph asked of it.
struct ActuatorChannel<A: SimulatedActuator> {
    device: A,
    commands: Subscriber<A::Command>,
    authority: Option<FixedSourceLease<A::Command>>,
    ready: Option<ParticipantReadyEvents>,
}

impl<A: SimulatedActuator> ActuatorChannel<A> {
    fn apply_backlog(&mut self) -> Result<()> {
        let Some(authority) = self.authority.as_mut() else {
            return self.device.apply_backlog(&self.commands);
        };
        if let Some(ready) = self.ready.as_ref() {
            while let Some(event) = ready.try_recv() {
                authority.update_ready_event(&event);
            }
            if ready.overflowed() {
                authority.mark_ready_overflow();
            }
        }
        let Some(host_now) = LocalInstant::try_now() else {
            // A receiver that cannot stamp its own clock cannot prove that a
            // retained motor command is still live.  Drain and park before
            // the next Webots step; the runner will surface the latched clock
            // fault as a participant failure.
            self.commands.take_all();
            authority.clear();
            return self.device.park();
        };
        match self.commands.take_newest_observed() {
            Some(observed) => {
                let body = observed.body;
                let accepted = matches!(
                    authority.offer(
                        observed.metadata.source.participant_source(),
                        observed.metadata.sequence,
                        observed.observed_at,
                        body.clone(),
                    ),
                    LeaseDecision::Acquired | LeaseDecision::Renewed
                );
                if accepted && authority.live_host(host_now).is_some() {
                    self.device.apply_body(body)
                } else {
                    self.device.park()
                }
            }
            None if authority.live_host(host_now).is_none() => self.device.park(),
            None => Ok(()),
        }
    }

    fn park(&mut self) -> Result<()> {
        // Whatever the graph sent that the stopped loop will never apply goes
        // with it; leaving it queued would apply it to a later world.
        self.commands.take_all();
        self.device.park()
    }
}

/// The battery device and the state handle it publishes on.
///
/// The battery is bound on its own: it is the one capability Webots hangs off
/// the robot rather than off a named device, and what it reports is state
/// rather than a measurement.
struct BatteryChannel {
    device: NativeBattery,
    publisher: StatePublisher<api::component::battery::State>,
}

/// One body a completed world advance produced, carrying the handle it
/// publishes on.
pub(crate) enum PendingPublish {
    Encoder(
        MeasurementPublisher<api::component::encoder::Sample>,
        api::component::encoder::Sample,
    ),
    Imu(
        MeasurementPublisher<api::component::imu::Sample>,
        api::component::imu::Sample,
    ),
    Accelerometer(
        MeasurementPublisher<api::component::accelerometer::Sample>,
        api::component::accelerometer::Sample,
    ),
    Gyroscope(
        MeasurementPublisher<api::component::gyroscope::Sample>,
        api::component::gyroscope::Sample,
    ),
    Range(
        MeasurementPublisher<api::component::range::Sample>,
        api::component::range::Sample,
    ),
    Camera(
        MeasurementPublisher<api::component::camera::Frame>,
        api::component::camera::Frame,
    ),
    Depth(
        MeasurementPublisher<api::component::depth::Frame>,
        api::component::depth::Frame,
    ),
    Gnss(
        MeasurementPublisher<api::component::gnss::Sample>,
        api::component::gnss::Sample,
    ),
    Magnetometer(
        MeasurementPublisher<api::component::magnetometer::Sample>,
        api::component::magnetometer::Sample,
    ),
    Lidar(
        MeasurementPublisher<api::component::lidar::Scan>,
        api::component::lidar::Scan,
    ),
    Mmwave(
        MeasurementPublisher<api::component::mmwave::Scan>,
        api::component::mmwave::Scan,
    ),
    Microphone(
        MeasurementPublisher<api::component::microphone::Frame>,
        api::component::microphone::Frame,
    ),
    Battery(
        StatePublisher<api::component::battery::State>,
        api::component::battery::State,
    ),
}

impl PendingPublish {
    /// Publish the body on the handle it was produced with.
    ///
    /// Simulated sensors read the world at exactly the instant the world
    /// advanced to, so their capture is exact rather than uncertain. A battery
    /// reports what the pack is, not what a sensor saw at an instant, so it is
    /// state stamped with the world step like the clock itself.
    fn publish(self, captured_at: CaptureStamp, world_step: &WorldStepToken) -> Result<()> {
        match self {
            Self::Encoder(publisher, body) => publisher.publish(captured_at, body)?,
            Self::Imu(publisher, body) => publisher.publish(captured_at, body)?,
            Self::Accelerometer(publisher, body) => publisher.publish(captured_at, body)?,
            Self::Gyroscope(publisher, body) => publisher.publish(captured_at, body)?,
            Self::Range(publisher, body) => publisher.publish(captured_at, body)?,
            Self::Camera(publisher, body) => publisher.publish(captured_at, body)?,
            Self::Depth(publisher, body) => publisher.publish(captured_at, body)?,
            Self::Gnss(publisher, body) => publisher.publish(captured_at, body)?,
            Self::Magnetometer(publisher, body) => publisher.publish(captured_at, body)?,
            Self::Lidar(publisher, body) => publisher.publish(captured_at, body)?,
            Self::Mmwave(publisher, body) => publisher.publish(captured_at, body)?,
            Self::Microphone(publisher, body) => publisher.publish(captured_at, body)?,
            Self::Battery(publisher, body) => publisher.publish(world_step, body)?,
        }
        Ok(())
    }
}

/// Everything one completed world advance produced, in the order the
/// controller's capabilities are bound.
pub(crate) struct StepOutput {
    pending: Vec<PendingPublish>,
}

impl StepOutput {
    /// The bodies one advance produced, in binding order.
    pub(crate) const fn new(pending: Vec<PendingPublish>) -> Self {
        Self { pending }
    }

    /// Publish every body this advance produced, each on its own handle.
    pub(crate) fn publish(self, world_step: &WorldStepToken) -> Result<()> {
        let captured_at = CaptureStamp::exact(world_step.instant());
        for pending in self.pending {
            pending.publish(captured_at, world_step)?;
        }
        Ok(())
    }
}

/// One bound capability: the reference it was declared under, and the device
/// and handle serving it.
pub(crate) struct CapabilityChannel {
    reference: CapabilityRef,
    binding: CapabilityBinding,
}

/// The device and handle behind one capability, statically typed by the family
/// it belongs to.
enum CapabilityBinding {
    Motor(ActuatorChannel<NativeMotor>),
    Led(ActuatorChannel<NativeLed>),
    Speaker(ActuatorChannel<NativeSpeaker>),
    Encoder(SensorChannel<NativeEncoder>),
    Imu(SensorChannel<NativeImu>),
    Accelerometer(SensorChannel<NativeAccelerometer>),
    Gyroscope(SensorChannel<NativeGyroscope>),
    Range(SensorChannel<NativeRange>),
    Camera(SensorChannel<NativeCamera>),
    Depth(SensorChannel<NativeDepth>),
    Gnss(SensorChannel<NativeGnss>),
    Magnetometer(SensorChannel<NativeMagnetometer>),
    Lidar(SensorChannel<NativeLidar>),
    Mmwave(SensorChannel<NativeMmwave>),
    Microphone(SensorChannel<NativeMicrophone>),
    Battery(BatteryChannel),
}

impl CapabilityChannel {
    /// Open this capability's Webots device and attach the bus handle it is
    /// served on.
    pub(crate) async fn bind(
        ctx: &mut SetupContext<WebotsControllerSimulator>,
        webots: &webots_rs::Webots,
        spec: &CapabilitySpec,
    ) -> Result<Self> {
        let reference = spec.reference().clone();
        // Every topic under this capability starts from the same component
        // segment; the leaf method is what names the contract.
        let component = || api::topic::owner().component(&reference.component_id);
        let id = &reference.capability_id;
        let binding = match spec {
            CapabilitySpec::Motor(spec) => CapabilityBinding::Motor(ActuatorChannel {
                device: NativeMotor::new(webots, spec)?,
                commands: ctx.subscriber(component().motor(id).command()).await?,
                authority: Some(FixedSourceLease::new(
                    "component/motor/command",
                    ParticipantId::new("drive")?,
                    MOTOR_SOURCE_SILENCE,
                    Duration::MAX,
                )),
                ready: Some(ctx.participant_ready_events().await?),
            }),
            CapabilitySpec::Led(led) => CapabilityBinding::Led(ActuatorChannel {
                device: NativeLed::new(webots, led)?,
                commands: ctx.subscriber(component().led(id).command()).await?,
                authority: None,
                ready: None,
            }),
            CapabilitySpec::Speaker(speaker) => CapabilityBinding::Speaker(ActuatorChannel {
                device: NativeSpeaker::new(webots, speaker)?,
                commands: ctx.subscriber(component().speaker(id).stream()).await?,
                authority: None,
                ready: None,
            }),
            CapabilitySpec::Encoder(spec) => CapabilityBinding::Encoder(SensorChannel {
                device: NativeEncoder::new(webots, spec)?,
                publisher: ctx.measurement_publisher(component().encoder(id).sample())?,
            }),
            CapabilitySpec::Imu(spec) => CapabilityBinding::Imu(SensorChannel {
                device: NativeImu::new(webots, spec)?,
                publisher: ctx.measurement_publisher(component().imu(id).sample())?,
            }),
            CapabilitySpec::Accelerometer(spec) => {
                CapabilityBinding::Accelerometer(SensorChannel {
                    device: NativeAccelerometer::new(webots, spec)?,
                    publisher: ctx.measurement_publisher(component().accelerometer(id).sample())?,
                })
            }
            CapabilitySpec::Gyroscope(spec) => CapabilityBinding::Gyroscope(SensorChannel {
                device: NativeGyroscope::new(webots, spec)?,
                publisher: ctx.measurement_publisher(component().gyroscope(id).sample())?,
            }),
            CapabilitySpec::Range(spec) => CapabilityBinding::Range(SensorChannel {
                device: NativeRange::new(webots, spec)?,
                publisher: ctx.measurement_publisher(component().range(id).sample())?,
            }),
            CapabilitySpec::Camera(spec) => CapabilityBinding::Camera(SensorChannel {
                device: NativeCamera::new(webots, spec)?,
                publisher: ctx.measurement_publisher(component().camera(id).frame())?,
            }),
            CapabilitySpec::Depth(spec) => CapabilityBinding::Depth(SensorChannel {
                device: NativeDepth::new(webots, spec)?,
                publisher: ctx.measurement_publisher(component().depth(id).frame())?,
            }),
            CapabilitySpec::Gnss(spec) => CapabilityBinding::Gnss(SensorChannel {
                device: NativeGnss::new(webots, spec)?,
                publisher: ctx.measurement_publisher(component().gnss(id).sample())?,
            }),
            CapabilitySpec::Magnetometer(spec) => CapabilityBinding::Magnetometer(SensorChannel {
                device: NativeMagnetometer::new(webots, spec)?,
                publisher: ctx.measurement_publisher(component().magnetometer(id).sample())?,
            }),
            CapabilitySpec::Lidar(spec) => CapabilityBinding::Lidar(SensorChannel {
                device: NativeLidar::new(webots, spec)?,
                publisher: ctx.measurement_publisher(component().lidar(id).scan())?,
            }),
            CapabilitySpec::Mmwave(spec) => CapabilityBinding::Mmwave(SensorChannel {
                device: NativeMmwave::new(webots, spec)?,
                publisher: ctx.measurement_publisher(component().mmwave(id).scan())?,
            }),
            CapabilitySpec::Microphone(spec) => CapabilityBinding::Microphone(SensorChannel {
                device: NativeMicrophone::new(webots, spec)?,
                publisher: ctx.measurement_publisher(component().microphone(id).frame())?,
            }),
            CapabilitySpec::Battery(spec) => CapabilityBinding::Battery(BatteryChannel {
                device: NativeBattery::new(spec)?,
                publisher: ctx.state_publisher(component().battery(id).state())?,
            }),
        };
        Ok(Self { reference, binding })
    }

    /// The capability this channel serves.
    pub(crate) const fn reference(&self) -> &CapabilityRef {
        &self.reference
    }

    /// Apply everything the graph asked this capability to do since the
    /// previous step. Sensors have nothing to apply: they are read after the
    /// world advances, not before.
    pub(crate) fn apply_backlog(&mut self) -> Result<()> {
        match &mut self.binding {
            CapabilityBinding::Motor(channel) => channel.apply_backlog(),
            CapabilityBinding::Led(channel) => channel.apply_backlog(),
            CapabilityBinding::Speaker(channel) => channel.apply_backlog(),
            CapabilityBinding::Encoder(_)
            | CapabilityBinding::Imu(_)
            | CapabilityBinding::Accelerometer(_)
            | CapabilityBinding::Gyroscope(_)
            | CapabilityBinding::Range(_)
            | CapabilityBinding::Camera(_)
            | CapabilityBinding::Depth(_)
            | CapabilityBinding::Gnss(_)
            | CapabilityBinding::Magnetometer(_)
            | CapabilityBinding::Lidar(_)
            | CapabilityBinding::Mmwave(_)
            | CapabilityBinding::Microphone(_)
            | CapabilityBinding::Battery(_) => Ok(()),
        }
    }

    /// Leave this capability quiet when the simulation stops. Sensors have
    /// nothing to quiet.
    pub(crate) fn park(&mut self) -> Result<()> {
        match &mut self.binding {
            CapabilityBinding::Motor(channel) => channel.park(),
            CapabilityBinding::Led(channel) => channel.park(),
            CapabilityBinding::Speaker(channel) => channel.park(),
            CapabilityBinding::Encoder(_)
            | CapabilityBinding::Imu(_)
            | CapabilityBinding::Accelerometer(_)
            | CapabilityBinding::Gyroscope(_)
            | CapabilityBinding::Range(_)
            | CapabilityBinding::Camera(_)
            | CapabilityBinding::Depth(_)
            | CapabilityBinding::Gnss(_)
            | CapabilityBinding::Magnetometer(_)
            | CapabilityBinding::Lidar(_)
            | CapabilityBinding::Mmwave(_)
            | CapabilityBinding::Microphone(_)
            | CapabilityBinding::Battery(_) => Ok(()),
        }
    }

    /// Re-anchor sensor schedules and clear state derived from the previous
    /// world history before the first sample on a rewound timeline.
    pub(crate) fn reset(&mut self, logical_time_ns: u64) -> Result<()> {
        match &mut self.binding {
            CapabilityBinding::Encoder(channel) => channel.reset(logical_time_ns),
            CapabilityBinding::Imu(channel) => channel.reset(logical_time_ns),
            CapabilityBinding::Accelerometer(channel) => channel.reset(logical_time_ns),
            CapabilityBinding::Gyroscope(channel) => channel.reset(logical_time_ns),
            CapabilityBinding::Range(channel) => channel.reset(logical_time_ns),
            CapabilityBinding::Camera(channel) => channel.reset(logical_time_ns),
            CapabilityBinding::Depth(channel) => channel.reset(logical_time_ns),
            CapabilityBinding::Gnss(channel) => channel.reset(logical_time_ns),
            CapabilityBinding::Magnetometer(channel) => channel.reset(logical_time_ns),
            CapabilityBinding::Lidar(channel) => channel.reset(logical_time_ns),
            CapabilityBinding::Mmwave(channel) => channel.reset(logical_time_ns),
            CapabilityBinding::Microphone(channel) => channel.reset(logical_time_ns),
            CapabilityBinding::Battery(channel) => channel.device.reset(logical_time_ns),
            CapabilityBinding::Motor(_)
            | CapabilityBinding::Led(_)
            | CapabilityBinding::Speaker(_) => Ok(()),
        }
    }

    /// Queue this capability's reading for `step`, when the step is one it
    /// publishes on. Actuators produce nothing.
    fn read_into(&mut self, step: SensorStep, pending: &mut Vec<PendingPublish>) -> Result<()> {
        match &mut self.binding {
            CapabilityBinding::Encoder(channel) => {
                channel.read_into(step, PendingPublish::Encoder, pending)
            }
            CapabilityBinding::Imu(channel) => {
                channel.read_into(step, PendingPublish::Imu, pending)
            }
            CapabilityBinding::Accelerometer(channel) => {
                channel.read_into(step, PendingPublish::Accelerometer, pending)
            }
            CapabilityBinding::Gyroscope(channel) => {
                channel.read_into(step, PendingPublish::Gyroscope, pending)
            }
            CapabilityBinding::Range(channel) => {
                channel.read_into(step, PendingPublish::Range, pending)
            }
            CapabilityBinding::Camera(channel) => {
                channel.read_into(step, PendingPublish::Camera, pending)
            }
            CapabilityBinding::Depth(channel) => {
                channel.read_into(step, PendingPublish::Depth, pending)
            }
            CapabilityBinding::Gnss(channel) => {
                channel.read_into(step, PendingPublish::Gnss, pending)
            }
            CapabilityBinding::Magnetometer(channel) => {
                channel.read_into(step, PendingPublish::Magnetometer, pending)
            }
            CapabilityBinding::Lidar(channel) => {
                channel.read_into(step, PendingPublish::Lidar, pending)
            }
            CapabilityBinding::Mmwave(channel) => {
                channel.read_into(step, PendingPublish::Mmwave, pending)
            }
            CapabilityBinding::Microphone(channel) => {
                channel.read_into(step, PendingPublish::Microphone, pending)
            }
            CapabilityBinding::Battery(channel) => {
                if let Some(state) = channel.device.read_if_due(step)? {
                    pending.push(PendingPublish::Battery(channel.publisher.clone(), state));
                }
                Ok(())
            }
            CapabilityBinding::Motor(_)
            | CapabilityBinding::Led(_)
            | CapabilityBinding::Speaker(_) => Ok(()),
        }
    }

    /// Read every channel for `step` into one publish queue.
    pub(crate) fn read_all(channels: &mut [Self], step: SensorStep) -> Result<StepOutput> {
        let mut pending = Vec::new();
        for channel in channels {
            channel.read_into(step, &mut pending)?;
        }
        Ok(StepOutput::new(pending))
    }
}
