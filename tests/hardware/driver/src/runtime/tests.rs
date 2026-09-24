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
use crate::api::__contracts::phoxal::fixture::hardware::v1::{
    FixtureObservation, FixtureSetpoint, hardware_fixture,
};
use phoxal::contract::{MethodDescriptor, MethodShape};
use phoxal::runtime::{
    Sample,
    input::{Samples, Setpoint},
};

const SETPOINT_VALID_FOR_MS: u64 = 50;

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
    fn freeze(&mut self, _candidate: &HardwareInvocation) -> phoxal::Result<HardwareFixtureInputs> {
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

    fn reserve(&mut self, outputs: &HardwareFixtureOutputs) -> phoxal::Result<Self::Reservation> {
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
fn generated_contract_owns_the_fixture_methods() {
    assert_eq!(
        hardware_fixture::methods::OBSERVATIONS.signature().endpoint,
        "observations"
    );
    assert_eq!(
        hardware_fixture::methods::ACTUATOR.signature().endpoint,
        "actuator"
    );
    assert_eq!(
        hardware_fixture::methods::OBSERVATIONS.signature().service,
        "phoxal.fixture.hardware.v1.HardwareFixture"
    );
    assert_eq!(
        hardware_fixture::methods::OBSERVATIONS.signature().shape,
        MethodShape::Observation
    );
    assert_eq!(
        hardware_fixture::methods::ACTUATOR.signature().shape,
        MethodShape::Observation
    );
    assert_eq!(
        hardware_fixture::methods::ACTUATOR
            .signature()
            .lease
            .expect("actuator lease")
            .valid_for_ms(),
        100
    );
    assert!(
        !hardware_fixture::methods::OBSERVATIONS
            .signature()
            .descriptor_set()
            .is_empty()
    );
    assert!(
        !hardware_fixture::methods::OBSERVATIONS
            .signature()
            .descriptor_set()
            .is_empty()
    );
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
