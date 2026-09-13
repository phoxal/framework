//! A production-shaped hardware Runtime fixture.
//!
//! The fixture has a real service-owned Protobuf contract and a normal
//! Runtime binary, but its device is an explicitly synthetic I/O source used
//! only for deterministic acceptance.  It does not model a DDSM115 or any
//! other physical device.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use phoxal::runtime::input::{Samples, Setpoint};
use phoxal::runtime::{InitContext, Runtime, Sample, StepContext};

/// Generated messages and typed ports owned by this fixture package.
pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/phoxal.fixture.hardware.v1.rs"));
}

pub use generated::*;

/// The original descriptor closure retained for independent artifact
/// inspection.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));

/// Public typed ports owned by the fixture contract.
pub use generated::hardware_fixture::ports;

/// Fixed validity interval used by the fixture's actuator path.
pub const SETPOINT_VALID_FOR_MS: u64 = 50;

/// Configuration for one selected fixture device.
#[derive(Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct HardwareFixtureConfig {
    /// Stable identity of the injected fixture device.
    pub device_id: String,
}

/// State retained by the fixture Runtime.
#[derive(Debug, Default)]
pub struct HardwareFixtureState {
    accepted_steps: u64,
    applied_velocity_radps: f64,
}

#[derive(Default)]
struct RuntimeControl {
    stalled: AtomicBool,
    stop_requested: AtomicBool,
}

/// A Runtime driver backed by the acceptance fixture's injected I/O.
///
/// The default value is used by the standalone binary.  Acceptance tests
/// clone the driver and use the private control handle to inject a stalled
/// computation or request a terminal stop; no production hardware behavior is
/// implied by those controls.
#[derive(Clone, Default)]
pub struct HardwareFixtureDriver {
    control: Arc<RuntimeControl>,
}

impl HardwareFixtureDriver {
    /// Creates a fixture driver with no injected fault.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Inputs acquired by the fixture driver at one Runtime boundary.
#[phoxal::runtime::inputs]
pub struct HardwareFixtureInputs {
    /// Samples obtained by the independent fixture acquisition loop.
    #[phoxal::runtime::input(max_items = 16, max_bytes = 4096)]
    pub observations: Samples<FixtureObservation>,
    /// Latest actuator intent received by the fixture transport.
    pub actuator: Setpoint<FixtureSetpoint>,
}

/// Fresh observations emitted by the fixture driver.
#[derive(Default)]
#[phoxal::runtime::outputs]
pub struct HardwareFixtureOutputs {
    /// Measured values obtained from the injected fixture device.
    #[phoxal::runtime::outputs::sample(
        port = ports::OBSERVATIONS,
        max_items = 16,
        max_bytes = 4096
    )]
    pub observations: Vec<Sample<FixtureObservation>>,
}

#[phoxal::runtime::outputs]
impl HardwareFixtureDriver {}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for HardwareFixtureDriver {
    type Config = HardwareFixtureConfig;
    type State = HardwareFixtureState;
    type Inputs = HardwareFixtureInputs;
    type Outputs = HardwareFixtureOutputs;

    fn init(&self, _ctx: &InitContext, config: Self::Config) -> Result<Self::State> {
        if config.device_id.trim().is_empty() {
            anyhow::bail!("fixture device_id must not be empty");
        }
        Ok(HardwareFixtureState::default())
    }

    fn step(
        &self,
        ctx: &StepContext,
        mut state: Self::State,
        inputs: &Self::Inputs,
    ) -> Result<(Self::State, Self::Outputs)> {
        while self.control.stalled.load(Ordering::Acquire) {
            if self.control.stop_requested.load(Ordering::Acquire) {
                anyhow::bail!("fixture computation stopped while stalled");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        if self.control.stop_requested.load(Ordering::Acquire) {
            anyhow::bail!("fixture computation stop requested");
        }

        state.accepted_steps = state.accepted_steps.saturating_add(1);
        state.applied_velocity_radps = inputs
            .actuator
            .value()
            .filter(|_| inputs.actuator.is_valid_at(ctx.now()))
            .map_or(0.0, |setpoint| setpoint.velocity_radps);
        let observations = inputs
            .observations
            .items()
            .iter()
            .map(|sample| Sample::new(*sample.payload(), sample.stamp().clone()))
            .collect();
        Ok((state, HardwareFixtureOutputs { observations }))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use phoxal::runtime::{
        AcceptedInvocation, ExecutionTime, HardwareInvocation, InputSource, ObservationStamp,
        OutputAdmission, OutputSink, PollOutcome, RuntimeRunner,
    };

    use super::*;

    const ACQUISITION_PERIOD: Duration = Duration::from_millis(2);
    const TRANSPORT_PERIOD: Duration = Duration::from_millis(1);
    const WATCHDOG_PERIOD: Duration = Duration::from_millis(1);
    const MAX_PENDING_OBSERVATIONS: usize = 16;

    struct OfferedSetpoint {
        value: FixtureSetpoint,
        issued_at: ExecutionTime,
        valid_until: ExecutionTime,
    }

    struct ActiveSetpoint {
        value: FixtureSetpoint,
        expires_at: Instant,
    }

    struct FixtureDevice {
        origin: Instant,
        stop: AtomicBool,
        observations: Mutex<VecDeque<Sample<FixtureObservation>>>,
        offered_setpoint: Mutex<Option<OfferedSetpoint>>,
        active_setpoint: Mutex<Option<ActiveSetpoint>>,
        acquisition_count: AtomicU64,
        transport_polls: AtomicU64,
        dropped_observations: AtomicU64,
        expiry_events: AtomicU64,
        published_observations: AtomicU64,
        workers: Mutex<Vec<JoinHandle<()>>>,
    }

    impl FixtureDevice {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                origin: Instant::now(),
                stop: AtomicBool::new(false),
                observations: Mutex::new(VecDeque::new()),
                offered_setpoint: Mutex::new(None),
                active_setpoint: Mutex::new(None),
                acquisition_count: AtomicU64::new(0),
                transport_polls: AtomicU64::new(0),
                dropped_observations: AtomicU64::new(0),
                expiry_events: AtomicU64::new(0),
                published_observations: AtomicU64::new(0),
                workers: Mutex::new(Vec::new()),
            })
        }

        fn start(self: &Arc<Self>) {
            let acquisition = Arc::clone(self);
            self.workers().push(thread::spawn(move || {
                while !acquisition.stop.load(Ordering::Acquire) {
                    let sequence = acquisition
                        .acquisition_count
                        .fetch_add(1, Ordering::AcqRel)
                        .saturating_add(1);
                    let sample = Sample::new(
                        FixtureObservation {
                            acquisition_sequence: sequence,
                            position_rad: sequence as f64 * 0.01,
                        },
                        ObservationStamp::new(
                            "fixture-device",
                            ExecutionTime::from(acquisition.origin.elapsed()),
                            Some(sequence),
                        ),
                    );
                    let mut observations = acquisition.lock(&acquisition.observations);
                    if observations.len() < MAX_PENDING_OBSERVATIONS {
                        observations.push_back(sample);
                    } else {
                        acquisition
                            .dropped_observations
                            .fetch_add(1, Ordering::AcqRel);
                    }
                    drop(observations);
                    thread::sleep(ACQUISITION_PERIOD);
                }
            }));

            let transport = Arc::clone(self);
            self.workers().push(thread::spawn(move || {
                while !transport.stop.load(Ordering::Acquire) {
                    transport.transport_polls.fetch_add(1, Ordering::AcqRel);
                    thread::sleep(TRANSPORT_PERIOD);
                }
            }));

            let watchdog = Arc::clone(self);
            self.workers().push(thread::spawn(move || {
                while !watchdog.stop.load(Ordering::Acquire) {
                    let expired = {
                        let active = watchdog.lock(&watchdog.active_setpoint);
                        active.as_ref().is_some_and(|value| {
                            let _ = value.value.velocity_radps;
                            Instant::now() >= value.expires_at
                        })
                    };
                    if expired {
                        *watchdog.lock(&watchdog.active_setpoint) = None;
                        watchdog.expiry_events.fetch_add(1, Ordering::AcqRel);
                    }
                    thread::sleep(WATCHDOG_PERIOD);
                }
            }));
        }

        fn stop(&self) {
            if self.stop.swap(true, Ordering::AcqRel) {
                return;
            }
            *self.lock(&self.active_setpoint) = None;
            *self.lock(&self.offered_setpoint) = None;
            let workers = std::mem::take(&mut *self.lock(&self.workers));
            for worker in workers {
                let _ = worker.join();
            }
        }

        fn workers(&self) -> MutexGuard<'_, Vec<JoinHandle<()>>> {
            self.lock(&self.workers)
        }

        fn lock<'a, T>(&self, value: &'a Mutex<T>) -> MutexGuard<'a, T> {
            value
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        fn offer_setpoint(
            &self,
            value: FixtureSetpoint,
            issued_at: ExecutionTime,
            valid_until: ExecutionTime,
            host_valid_for: Duration,
        ) {
            *self.lock(&self.offered_setpoint) = Some(OfferedSetpoint {
                value,
                issued_at,
                valid_until,
            });
            *self.lock(&self.active_setpoint) = Some(ActiveSetpoint {
                value,
                expires_at: Instant::now() + host_valid_for,
            });
        }

        fn input_setpoint(&self) -> Setpoint<FixtureSetpoint> {
            if !self.active_setpoint() {
                return Setpoint::withdrawn();
            }
            self.lock(&self.offered_setpoint)
                .as_ref()
                .map_or_else(Setpoint::withdrawn, |offered| {
                    Setpoint::from_parts(offered.value, offered.issued_at, offered.valid_until)
                })
        }

        fn drain_observations(&self) -> Samples<FixtureObservation> {
            let values = self.lock(&self.observations).drain(..).collect();
            if self.dropped_observations.swap(0, Ordering::AcqRel) > 0 {
                Samples::with_gap(values)
            } else {
                Samples::new(values)
            }
        }

        fn active_setpoint(&self) -> bool {
            self.lock(&self.active_setpoint).is_some()
        }

        fn acquisition_count(&self) -> u64 {
            self.acquisition_count.load(Ordering::Acquire)
        }

        fn transport_polls(&self) -> u64 {
            self.transport_polls.load(Ordering::Acquire)
        }

        fn dropped_observations(&self) -> u64 {
            self.dropped_observations.load(Ordering::Acquire)
        }

        fn expiry_events(&self) -> u64 {
            self.expiry_events.load(Ordering::Acquire)
        }
    }

    impl Drop for FixtureDevice {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
        }
    }

    struct FixtureInputSource {
        device: Arc<FixtureDevice>,
    }

    impl InputSource<HardwareFixtureDriver> for FixtureInputSource {
        fn freeze(
            &mut self,
            _candidate: &HardwareInvocation,
        ) -> phoxal::Result<HardwareFixtureInputs> {
            Ok(HardwareFixtureInputs {
                observations: self.device.drain_observations(),
                actuator: self.device.input_setpoint(),
            })
        }

        fn stop(&mut self) -> phoxal::Result<()> {
            self.device.stop();
            Ok(())
        }
    }

    struct FixtureOutputSink {
        device: Arc<FixtureDevice>,
        stopped: bool,
    }

    impl OutputAdmission<HardwareFixtureOutputs> for FixtureOutputSink {
        type Reservation = usize;

        fn reserve(
            &mut self,
            outputs: &HardwareFixtureOutputs,
        ) -> phoxal::Result<Self::Reservation> {
            if outputs.observations.len() > 16 {
                anyhow::bail!("fixture output capacity exhausted");
            }
            Ok(outputs.observations.len())
        }
    }

    impl OutputSink<HardwareFixtureDriver> for FixtureOutputSink {
        fn publish(
            &mut self,
            accepted: AcceptedInvocation<HardwareFixtureOutputs, Self::Reservation>,
        ) -> phoxal::Result<()> {
            self.device.published_observations.fetch_add(
                accepted.outputs().observations.len() as u64,
                Ordering::AcqRel,
            );
            Ok(())
        }

        fn stop(&mut self) -> phoxal::Result<()> {
            self.stopped = true;
            self.device.stop();
            Ok(())
        }
    }

    fn runner(
        driver: HardwareFixtureDriver,
        device: Arc<FixtureDevice>,
    ) -> RuntimeRunner<HardwareFixtureDriver, FixtureInputSource, FixtureOutputSink> {
        RuntimeRunner::new(
            driver,
            ExecutionTime::default(),
            HardwareFixtureConfig {
                device_id: "fixture-0".to_owned(),
            },
            FixtureInputSource {
                device: Arc::clone(&device),
            },
            FixtureOutputSink {
                device,
                stopped: false,
            },
        )
        .expect("fixture Runtime initializes")
    }

    #[test]
    fn generated_contract_owns_the_fixture_ports() {
        assert_eq!(ports::OBSERVATIONS.name(), "observations");
        assert_eq!(ports::ACTUATOR.name(), "actuator");
        assert_eq!(
            ports::OBSERVATIONS.signature().service,
            "phoxal.fixture.hardware.v1.HardwareFixture"
        );
        assert_eq!(
            ports::OBSERVATIONS.signature().kind,
            phoxal_port::PortKind::Sample
        );
        assert_eq!(
            ports::ACTUATOR.signature().kind,
            phoxal_port::PortKind::Setpoint
        );
        assert!(!FILE_DESCRIPTOR_SET.is_empty());
        assert!(!ports::OBSERVATIONS.signature().descriptor_set().is_empty());
    }

    #[test]
    fn hardware_acquisition_and_transport_continue_during_stalled_compute() {
        let device = FixtureDevice::new();
        device.start();
        let driver = HardwareFixtureDriver::new();
        driver.control.stalled.store(true, Ordering::Release);
        let mut runner = runner(driver.clone(), Arc::clone(&device));
        let task = thread::spawn(move || runner.poll(ExecutionTime::default()));

        thread::sleep(Duration::from_millis(60));
        let acquired = device.acquisition_count();
        let transport_polls = device.transport_polls();
        assert!(
            acquired >= 10,
            "acquisition stalled with computation: {acquired}"
        );
        assert!(
            transport_polls >= 20,
            "transport stalled with computation: {transport_polls}"
        );
        assert!(
            device.dropped_observations() > 0,
            "the fixture queue never reached its explicit bounded limit"
        );

        driver.control.stalled.store(false, Ordering::Release);
        let outcome = task.join().expect("fixture poll thread joins");
        assert!(matches!(outcome, Ok(PollOutcome::Accepted { .. })));
        assert!(device.acquisition_count() >= acquired);
        device.stop();
    }

    #[test]
    fn actuator_expiry_is_independent_of_stalled_compute() {
        let device = FixtureDevice::new();
        device.start();
        device.offer_setpoint(
            FixtureSetpoint {
                velocity_radps: 1.0,
            },
            ExecutionTime::default(),
            ExecutionTime::from(Duration::from_millis(SETPOINT_VALID_FOR_MS)),
            Duration::from_millis(SETPOINT_VALID_FOR_MS / 2),
        );
        let driver = HardwareFixtureDriver::new();
        driver.control.stalled.store(true, Ordering::Release);
        let mut runner = runner(driver.clone(), Arc::clone(&device));
        let task = thread::spawn(move || runner.poll(ExecutionTime::default()));

        thread::sleep(Duration::from_millis(70));
        assert!(!device.active_setpoint(), "expired intent remained active");
        assert!(
            device.expiry_events() >= 1,
            "watchdog never enforced expiry"
        );
        assert!(device.acquisition_count() >= 20);
        assert!(device.transport_polls() >= 40);

        driver.control.stalled.store(false, Ordering::Release);
        let outcome = task.join().expect("fixture poll thread joins");
        assert!(matches!(outcome, Ok(PollOutcome::Accepted { .. })));
        device.stop();
    }

    #[test]
    fn stalled_runtime_stop_is_bounded_and_does_not_rearm_actuation() {
        let device = FixtureDevice::new();
        device.start();
        device.offer_setpoint(
            FixtureSetpoint {
                velocity_radps: 2.0,
            },
            ExecutionTime::default(),
            ExecutionTime::from(Duration::from_millis(50)),
            Duration::from_secs(1),
        );
        let driver = HardwareFixtureDriver::new();
        driver.control.stalled.store(true, Ordering::Release);
        let mut runner = runner(driver.clone(), Arc::clone(&device));
        let task = thread::spawn(move || runner.poll(ExecutionTime::default()));

        thread::sleep(Duration::from_millis(25));
        let started = Instant::now();
        driver.control.stop_requested.store(true, Ordering::Release);
        let result = task.join().expect("stalled fixture poll thread joins");
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(
            result.is_err(),
            "stop request must fault the stalled invocation"
        );
        assert!(!device.active_setpoint(), "stop path re-armed the actuator");
        assert!(device.transport_polls() >= 10);
        device.stop();
    }

    #[test]
    fn invalid_fixture_configuration_fails_before_runtime_ready() {
        let error = phoxal::runtime::initialize(
            &HardwareFixtureDriver::new(),
            ExecutionTime::default(),
            HardwareFixtureConfig {
                device_id: "".to_owned(),
            },
        )
        .expect_err("missing configured fixture device must reject initialization");
        assert!(error.to_string().contains("device_id"));
    }
}
