//! Loopback-only private host server for native Webots controllers.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::state::{NativeRobotFailure, NativeWorldFailure, NativeWorldState};
use phoxal_simulator_webots_shared::plan::RobotSimulationPlan;
use phoxal_simulator_webots_shared::protocol::{
    ActuationEvidence, ControllerEvent, ControllerRole, HostDirective, HostRequest, HostResponse,
    NativeMutation, read_frame, write_frame,
};

const ACCEPT_POLL: Duration = Duration::from_millis(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const CONTROLLER_LIVENESS_TIMEOUT: Duration = Duration::from_secs(30);
const MUTATION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ACTUATION_RECORDS_PER_ROBOT: usize = 256;

/// One loopback listener and its validated native-world state.
pub struct HostServer {
    endpoint: String,
    state: Arc<Mutex<NativeWorldState>>,
    plans: Arc<Mutex<BTreeMap<String, RobotSimulationPlan>>>,
    mutation: Arc<(Mutex<MutationState>, Condvar)>,
    retiring: Arc<Mutex<BTreeSet<String>>>,
    actuation: Arc<Mutex<BTreeMap<String, ActuationBuffer>>>,
    controller_threads: Arc<Mutex<Vec<JoinHandle<()>>>>,
    connections: Arc<Mutex<BTreeMap<u64, TcpStream>>>,
    stop: Arc<AtomicBool>,
    acceptor: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct MutationState {
    next_transaction: u64,
    pending: Option<PendingMutation>,
}

struct PendingMutation {
    mutation: NativeMutation,
    result: Option<Result<(), String>>,
}

#[derive(Default)]
struct ActuationBuffer {
    records: VecDeque<ActuationEvidence>,
    dropped: u64,
}

impl ActuationBuffer {
    fn push(&mut self, record: ActuationEvidence) {
        if self.records.len() == MAX_ACTUATION_RECORDS_PER_ROBOT {
            self.records.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.records.push_back(record);
    }
}

impl HostServer {
    /// Bind an ephemeral loopback endpoint and begin accepting controllers.
    pub fn bind() -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .context("failed to bind the private Webots host endpoint")?;
        listener
            .set_nonblocking(true)
            .context("failed to make the private Webots host endpoint nonblocking")?;
        let address = listener
            .local_addr()
            .context("failed to read the private Webots host endpoint")?;
        let endpoint = format!("tcp://{address}");
        let state = Arc::new(Mutex::new(NativeWorldState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let plans = Arc::new(Mutex::new(BTreeMap::new()));
        let mutation = Arc::new((Mutex::new(MutationState::default()), Condvar::new()));
        let retiring = Arc::new(Mutex::new(BTreeSet::new()));
        let actuation = Arc::new(Mutex::new(BTreeMap::new()));
        let controller_threads = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(Mutex::new(BTreeMap::new()));
        let server_state = Arc::clone(&state);
        let server_plans = Arc::clone(&plans);
        let server_mutation = Arc::clone(&mutation);
        let server_retiring = Arc::clone(&retiring);
        let server_actuation = Arc::clone(&actuation);
        let server_controller_threads = Arc::clone(&controller_threads);
        let server_connections = Arc::clone(&connections);
        let server_stop = Arc::clone(&stop);
        let acceptor = std::thread::Builder::new()
            .name("webots-host-accept".to_owned())
            .spawn(move || {
                accept_loop(
                    listener,
                    &server_state,
                    &server_plans,
                    &server_mutation,
                    &server_retiring,
                    &server_actuation,
                    &server_controller_threads,
                    &server_connections,
                    &server_stop,
                );
            })
            .context("failed to start the private Webots host listener")?;
        Ok(Self {
            endpoint,
            state,
            plans,
            mutation,
            retiring,
            actuation,
            controller_threads,
            connections,
            stop,
            acceptor: Some(acceptor),
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    #[must_use]
    pub fn snapshot(&self) -> NativeWorldState {
        lock(&self.state).clone()
    }

    /// Apply the host-monotonic stopped-answering deadline to synchronized native roles.
    pub fn enforce_liveness(&self) {
        let world_mutation_active = lock(&self.mutation.0)
            .pending
            .as_ref()
            .is_some_and(|pending| pending.result.is_none());
        lock(&self.state).enforce_liveness(
            std::time::Instant::now(),
            CONTROLLER_LIVENESS_TIMEOUT,
            world_mutation_active,
        );
    }

    /// Reserve the exact derived plan before any Robot may join the native barrier.
    pub fn reserve_robot(
        &self,
        execution: phoxal::identity::ExecutionId,
        plan: RobotSimulationPlan,
    ) -> Result<(), NativeWorldFailure> {
        let execution = execution.to_string();
        let mut plans = lock(&self.plans);
        if plans.insert(execution.clone(), plan).is_some() {
            return Err(NativeWorldFailure::DuplicateRobot { execution });
        }
        Ok(())
    }

    /// Release a reservation after rollback or completed removal.
    pub fn release_robot(&self, execution: phoxal::identity::ExecutionId) {
        lock(&self.state).release_robot(execution);
        lock(&self.plans).remove(&execution.to_string());
    }

    #[must_use]
    pub fn robot_controller(
        &self,
        execution: phoxal::identity::ExecutionId,
    ) -> Option<phoxal::identity::ProducerId> {
        lock(&self.state).robot_controller(execution)
    }

    #[must_use]
    #[cfg(test)]
    pub fn has_robot(&self, execution: phoxal::identity::ExecutionId) -> bool {
        lock(&self.state).has_robot(execution)
    }

    #[must_use]
    pub fn robot_active_revision(&self, execution: phoxal::identity::ExecutionId) -> Option<u64> {
        lock(&self.state).robot_active_revision(execution)
    }

    /// Request an execution-specific cooperative park before native removal.
    pub fn retire_robot(&self, execution: phoxal::identity::ExecutionId) {
        lock(&self.retiring).insert(execution.to_string());
    }

    #[must_use]
    pub fn robot_is_parked(&self, execution: phoxal::identity::ExecutionId) -> bool {
        lock(&self.state).robot_is_parked(execution)
    }

    #[must_use]
    pub fn robot_failure(
        &self,
        execution: phoxal::identity::ExecutionId,
    ) -> Option<NativeRobotFailure> {
        lock(&self.state).robot_failure(execution)
    }

    /// Drain the bounded applied-action record when durable member evidence is written.
    pub fn take_actuation_evidence(
        &self,
        execution: phoxal::identity::ExecutionId,
    ) -> (Vec<ActuationEvidence>, u64) {
        let buffer = lock(&self.actuation)
            .remove(&execution.to_string())
            .unwrap_or_default();
        (buffer.records.into_iter().collect(), buffer.dropped)
    }

    /// Import one fully rendered Robot while the shared native world is paused.
    pub fn import_robot(
        &self,
        execution: phoxal::identity::ExecutionId,
        definition: String,
        source: String,
    ) -> Result<()> {
        phoxal_simulator_webots_shared::protocol::validate_robot_import(&definition, &source)?;
        self.mutate(|transaction| NativeMutation::ImportRobot {
            transaction,
            execution,
            definition,
            source,
        })
    }

    /// Remove one imported Robot during rollback or orderly detachment.
    pub fn remove_robot(&self, definition: String) -> Result<()> {
        self.mutate(|transaction| NativeMutation::RemoveRobot {
            transaction,
            definition,
        })
    }

    /// Idempotently remove any residue after an import attempt with an uncertain outcome.
    pub fn rollback_robot(&self, definition: String) -> Result<()> {
        self.mutate(|transaction| NativeMutation::RollbackRobot {
            transaction,
            definition,
        })
    }

    /// Request the only supported Live native motion policy.
    pub fn request_motion(
        &self,
        motion: phoxal_simulator_webots_shared::protocol::NativeMotion,
    ) -> Result<(), NativeWorldFailure> {
        lock(&self.state).request_motion(motion).map(|_| ())
    }

    /// Begin orderly world stop.
    pub fn stop_world(&self) {
        lock(&self.state).stop();
    }

    /// Whether the world controller acknowledged the host terminal directive.
    #[must_use]
    pub fn world_is_stopped(&self) -> bool {
        lock(&self.state).world_is_stopped()
    }

    /// Whether a world controller joined this native world at any point.
    #[must_use]
    pub fn has_world_controller(&self) -> bool {
        lock(&self.state).has_world_controller()
    }

    fn mutate(&self, build: impl FnOnce(u64) -> NativeMutation) -> Result<()> {
        let (mutex, completed) = &*self.mutation;
        let mut state = lock(mutex);
        anyhow::ensure!(
            state.pending.is_none(),
            "another native scene mutation is in progress"
        );
        state.next_transaction = state
            .next_transaction
            .checked_add(1)
            .context("native mutation counter exhausted")?;
        let transaction = state.next_transaction;
        state.pending = Some(PendingMutation {
            mutation: build(transaction),
            result: None,
        });
        let (mut state, timeout) = completed
            .wait_timeout_while(state, MUTATION_TIMEOUT, |state| {
                state
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.result.is_none())
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if timeout.timed_out() {
            state.pending = None;
            anyhow::bail!("native scene mutation timed out after {MUTATION_TIMEOUT:?}");
        }
        let pending = state
            .pending
            .take()
            .context("native mutation completion disappeared")?;
        pending
            .result
            .context("native mutation has no result")?
            .map_err(anyhow::Error::msg)?;
        Ok(())
    }
}

impl Drop for HostServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
        for (_, connection) in std::mem::take(&mut *lock(&self.connections)) {
            let _ = connection.shutdown(Shutdown::Both);
        }
        for controller in std::mem::take(&mut *lock(&self.controller_threads)) {
            let _ = controller.join();
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the private listener shares six separately synchronized bounded authorities"
)]
fn accept_loop(
    listener: TcpListener,
    state: &Arc<Mutex<NativeWorldState>>,
    plans: &Arc<Mutex<BTreeMap<String, RobotSimulationPlan>>>,
    mutation: &Arc<(Mutex<MutationState>, Condvar)>,
    retiring: &Arc<Mutex<BTreeSet<String>>>,
    actuation: &Arc<Mutex<BTreeMap<String, ActuationBuffer>>>,
    controller_threads: &Arc<Mutex<Vec<JoinHandle<()>>>>,
    connections: &Arc<Mutex<BTreeMap<u64, TcpStream>>>,
    stop: &Arc<AtomicBool>,
) {
    let mut next_connection = 0_u64;
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                let shutdown_connection = match stream.try_clone() {
                    Ok(connection) => connection,
                    Err(error) => {
                        lock(state).protocol_failure(format!(
                            "failed to retain private controller shutdown authority: {error}"
                        ));
                        continue;
                    }
                };
                let Some(connection_id) = next_connection.checked_add(1) else {
                    let _ = shutdown_connection.shutdown(Shutdown::Both);
                    lock(state).protocol_failure(
                        "private controller connection identity exhausted".to_owned(),
                    );
                    return;
                };
                next_connection = connection_id;
                lock(connections).insert(connection_id, shutdown_connection);
                let worker_state = Arc::clone(state);
                let plans = Arc::clone(plans);
                let mutation = Arc::clone(mutation);
                let retiring = Arc::clone(retiring);
                let actuation = Arc::clone(actuation);
                let worker_connections = Arc::clone(connections);
                match std::thread::Builder::new()
                    .name("webots-host-controller".to_owned())
                    .spawn(move || {
                        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            serve_controller(
                                stream,
                                &worker_state,
                                &plans,
                                &mutation,
                                &retiring,
                                &actuation,
                            )
                        }));
                        match outcome {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => {
                                tracing::warn!(error = %error, "private Webots controller link ended");
                                lock(worker_state.as_ref()).protocol_failure(format!(
                                    "private Webots controller link failed: {error:#}"
                                ));
                            }
                            Err(_) => lock(worker_state.as_ref()).protocol_failure(
                                "private Webots controller worker panicked".to_owned(),
                            ),
                        }
                        if let Some(connection) =
                            lock(worker_connections.as_ref()).remove(&connection_id)
                        {
                            let _ = connection.shutdown(Shutdown::Both);
                        }
                    })
                {
                    Ok(thread) => lock(controller_threads).push(thread),
                    Err(error) => {
                        if let Some(connection) = lock(connections).remove(&connection_id) {
                            let _ = connection.shutdown(Shutdown::Both);
                        }
                        lock(state.as_ref()).protocol_failure(format!(
                            "failed to spawn private Webots controller worker: {error}"
                        ));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(error) => {
                tracing::error!(error = %error, "private Webots host listener failed");
                lock(state)
                    .protocol_failure(format!("private Webots host listener failed: {error}"));
                return;
            }
        }
    }
}

fn serve_controller(
    mut stream: TcpStream,
    state: &Arc<Mutex<NativeWorldState>>,
    plans: &Arc<Mutex<BTreeMap<String, RobotSimulationPlan>>>,
    mutation: &Arc<(Mutex<MutationState>, Condvar)>,
    retiring: &Arc<Mutex<BTreeSet<String>>>,
    actuation: &Arc<Mutex<BTreeMap<String, ActuationBuffer>>>,
) -> Result<()> {
    stream
        .set_nonblocking(false)
        .context("failed to make a private Webots controller connection blocking")?;
    stream
        .set_nodelay(true)
        .context("failed to configure a private Webots controller connection")?;
    stream
        .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .context("failed to bound private Webots controller reads")?;
    let hello = read_frame::<_, HostRequest>(&mut stream)?;
    let HostRequest::Hello { framework, role } = hello else {
        write_frame(
            &mut stream,
            &HostResponse::Rejected {
                reason: "the first private host message must be Hello".to_owned(),
            },
        )?;
        anyhow::bail!("the first private host message was not Hello");
    };
    let robot_plan = match role {
        ControllerRole::World => None,
        ControllerRole::Robot { execution } => {
            let plan = lock(plans).get(&execution.to_string()).cloned();
            if plan.is_none() {
                write_frame(
                    &mut stream,
                    &HostResponse::Rejected {
                        reason: "this execution has no fully validated RobotSimulationPlan"
                            .to_owned(),
                    },
                )?;
                return Ok(());
            }
            plan
        }
    };
    let admitted = lock(state).admit(framework, role);
    match admitted {
        Ok(directive) => write_frame(
            &mut stream,
            &HostResponse::Accepted {
                directive,
                robot_plan,
            },
        )?,
        Err(error) => {
            write_frame(
                &mut stream,
                &HostResponse::Rejected {
                    reason: format!("{error:?}"),
                },
            )?;
            return Ok(());
        }
    }
    stream
        .set_read_timeout(None)
        .context("failed to remove the admitted controller handshake timeout")?;

    let outcome = (|| -> Result<()> {
        loop {
            match read_frame::<_, HostRequest>(&mut stream)? {
                HostRequest::Event(event) => {
                    if !event_allowed(role, &event) {
                        write_frame(
                            &mut stream,
                            &HostResponse::Rejected {
                                reason: "the controller event does not match its admitted role"
                                    .to_owned(),
                            },
                        )?;
                        lock(state).protocol_failure(
                            "a controller published an event outside its admitted role".to_owned(),
                        );
                        return Ok(());
                    }
                    if let ControllerEvent::MutationCompleted { transaction, error } = &event {
                        complete_mutation(mutation, *transaction, error.clone())?;
                    }
                    if let ControllerEvent::RobotImported { transaction } = &event {
                        let mut state = lock(&mutation.0);
                        let pending = state
                            .pending
                            .as_mut()
                            .context("Robot imported outside a mutation")?;
                        let NativeMutation::ImportRobot {
                            execution,
                            transaction: expected,
                            ..
                        } = &pending.mutation
                        else {
                            anyhow::bail!("Robot imported outside the import phase");
                        };
                        anyhow::ensure!(
                            transaction == expected,
                            "Robot import transaction mismatch"
                        );
                        pending.mutation = NativeMutation::StartRobotController {
                            transaction: *transaction,
                            execution: *execution,
                            ready: false,
                        };
                    }
                    if let (
                        ControllerRole::Robot { execution },
                        ControllerEvent::ActuationEvidence(records),
                    ) = (role, &event)
                    {
                        let mut evidence = lock(actuation);
                        let retained = evidence.entry(execution.to_string()).or_default();
                        for record in records {
                            retained.push(record.clone());
                        }
                    }
                    let mut native = lock(state);
                    let response = match native.observe(role, event) {
                        Ok(directive) => HostResponse::Directive(directive_for(
                            role, directive, mutation, retiring, &native,
                        )),
                        Err(error) => HostResponse::Rejected {
                            reason: format!("{error:?}"),
                        },
                    };
                    drop(native);
                    write_frame(&mut stream, &response)?;
                }
                HostRequest::Hello { .. } => {
                    write_frame(
                        &mut stream,
                        &HostResponse::Rejected {
                            reason: "a controller may handshake only once".to_owned(),
                        },
                    )?;
                    lock(state).protocol_failure(
                        "an admitted controller attempted a second handshake".to_owned(),
                    );
                    return Ok(());
                }
            }
        }
    })();
    // Every admitted controller owns one native synchronization role. Remove the directive
    // tombstone at connection teardown, then let the state machine distinguish a parked/released
    // retirement from an unexpected synchronized-controller loss.
    if let ControllerRole::Robot { execution } = role {
        lock(retiring).remove(&execution.to_string());
    }
    lock(state).controller_lost(role);
    if role == ControllerRole::World {
        let mut pending = lock(&mutation.0);
        if let Some(pending) = &mut pending.pending
            && pending.result.is_none()
        {
            pending.result = Some(Err(
                "world controller disconnected during mutation".to_owned()
            ));
            mutation.1.notify_all();
        }
    }
    if let Err(error) = outcome {
        tracing::warn!(error = %error, ?role, "classified private Webots controller link ended");
    }
    Ok(())
}

fn directive_for(
    role: ControllerRole,
    fallback: HostDirective,
    mutation: &Arc<(Mutex<MutationState>, Condvar)>,
    retiring: &Arc<Mutex<BTreeSet<String>>>,
    native: &NativeWorldState,
) -> HostDirective {
    if matches!(fallback, HostDirective::Stop { .. }) {
        return fallback;
    }
    if let ControllerRole::Robot { execution } = role
        && lock(retiring).contains(&execution.to_string())
    {
        return HostDirective::Stop {
            reason: "the Robot attachment is being rolled back".to_owned(),
        };
    }
    if role != ControllerRole::World {
        return fallback;
    }
    lock(&mutation.0)
        .pending
        .as_ref()
        .filter(|pending| pending.result.is_none())
        .map_or(fallback, |pending| {
            let mut mutation = pending.mutation.clone();
            if let NativeMutation::StartRobotController {
                execution, ready, ..
            } = &mut mutation
            {
                *ready = native.robot_controller(*execution).is_some();
            }
            HostDirective::Mutate(mutation)
        })
}

fn complete_mutation(
    mutation: &Arc<(Mutex<MutationState>, Condvar)>,
    transaction: u64,
    error: Option<String>,
) -> Result<()> {
    let mut state = lock(&mutation.0);
    let pending = state
        .pending
        .as_mut()
        .context("world controller completed no pending mutation")?;
    anyhow::ensure!(
        pending.mutation.transaction() == transaction,
        "world controller completed mutation {transaction}, expected {}",
        pending.mutation.transaction()
    );
    anyhow::ensure!(pending.result.is_none(), "native mutation completed twice");
    pending.result = Some(error.map_or(Ok(()), Err));
    mutation.1.notify_all();
    Ok(())
}

const fn event_allowed(role: ControllerRole, event: &ControllerEvent) -> bool {
    match role {
        ControllerRole::World => matches!(
            event,
            ControllerEvent::Heartbeat
                | ControllerEvent::WorldReady { .. }
                | ControllerEvent::WorldMode { .. }
                | ControllerEvent::WorldProgress(_)
                | ControllerEvent::MutationCompleted { .. }
                | ControllerEvent::RobotImported { .. }
                | ControllerEvent::Stopped
                | ControllerEvent::Fault(_)
        ),
        ControllerRole::Robot { .. } => matches!(
            event,
            ControllerEvent::Heartbeat
                | ControllerEvent::RobotReady { .. }
                | ControllerEvent::RobotActive { .. }
                | ControllerEvent::RobotBoundary { .. }
                | ControllerEvent::RobotParked
                | ControllerEvent::RobotStopping
                | ControllerEvent::RobotSupervisorLost
                | ControllerEvent::ActuationEvidence(_)
                | ControllerEvent::Stopped
                | ControllerEvent::Fault(_)
        ),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::bus::RobotInstant;
    use phoxal::identity::TimelineId;
    use phoxal::model::identity::{CapabilityId, CapabilityRef, ComponentInstanceId};
    use phoxal::model::world::WorldProgress;
    use phoxal_simulator_webots_shared::protocol::{
        ActuationSelection, AppliedActuation, ControllerEvent, ControllerLink, ControllerRole,
        NoActuationReason, ObservedNativeMode,
    };

    fn execution(value: u128) -> phoxal::identity::ExecutionId {
        phoxal::identity::ExecutionId::try_from(value).expect("execution")
    }

    fn empty_plan(robot: &str) -> RobotSimulationPlan {
        RobotSimulationPlan {
            robot: robot.to_owned(),
            basic_time_step_ms: 12,
            substitutions: Vec::new(),
            capabilities: Vec::new(),
            links: Vec::new(),
            assets: Vec::new(),
        }
    }

    fn wait_for_robot_release(server: &HostServer, execution: phoxal::identity::ExecutionId) {
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while server.has_robot(execution) {
            assert!(
                std::time::Instant::now() < deadline,
                "controller did not release its native state"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn one_world_controller_handshakes_and_reports_ready() {
        let server = HostServer::bind().expect("the loopback host binds");
        let link = ControllerLink::connect(server.endpoint(), ControllerRole::World)
            .expect("the world controller connects");
        link.publish(ControllerEvent::WorldReady {
            time_step_ns: 12_000_000,
            mode: ObservedNativeMode::Paused,
        })
        .expect("the ready event enters the bounded queue");
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            match link.directive() {
                Err(error) => panic!(
                    "private link failed: {error}; native snapshot: {:?}",
                    server.snapshot()
                ),
                Ok(HostDirective::Continue {
                    motion: phoxal_simulator_webots_shared::protocol::NativeMotion::Paused,
                }) => break,
                Ok(HostDirective::Park) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(directive) => panic!("unexpected private directive: {directive:?}"),
            }
        }
        let snapshot = server.snapshot();
        assert!(
            matches!(
                snapshot.lifecycle(),
                crate::state::NativeWorldLifecycle::Ready { .. }
            ),
            "unexpected native snapshot: {snapshot:?}"
        );
    }

    #[test]
    fn world_controller_loss_wakes_an_in_flight_mutation() {
        let server = Arc::new(HostServer::bind().expect("host binds"));
        let link = ControllerLink::connect(server.endpoint(), ControllerRole::World)
            .expect("world controller connects");
        link.exchange(ControllerEvent::WorldReady {
            time_step_ns: 12_000_000,
            mode: ObservedNativeMode::Paused,
        })
        .expect("world ready");
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn({
            let server = Arc::clone(&server);
            move || {
                sender
                    .send(server.import_robot(
                        execution(0x1000_0000_0000_0000_0000_0000_0000_0001),
                        "ROBOT".to_owned(),
                        "Robot {}".to_owned(),
                    ))
                    .expect("result delivered")
            }
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while lock(&server.mutation.0).pending.is_none() {
            assert!(std::time::Instant::now() < deadline, "mutation starts");
            std::thread::yield_now();
        }
        drop(link);
        let error = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("loss wakes mutation promptly")
            .expect_err("lost controller cannot import");
        assert!(error.to_string().contains("disconnected"));
        worker.join().expect("worker completes");
        assert!(matches!(
            server.snapshot().lifecycle(),
            crate::state::NativeWorldLifecycle::Failed(
                crate::state::NativeWorldFailure::WorldControllerLost
            )
        ));
    }

    #[test]
    fn imported_scene_releases_source_before_zero_time_controller_bootstrap() {
        let server = Arc::new(HostServer::bind().expect("host binds"));
        let world =
            ControllerLink::connect(server.endpoint(), ControllerRole::World).expect("world link");
        world
            .exchange(ControllerEvent::WorldReady {
                time_step_ns: 12_000_000,
                mode: ObservedNativeMode::Paused,
            })
            .expect("world ready");
        let execution = execution(0x1000_0000_0000_0000_0000_0000_0000_0001);
        server
            .reserve_robot(execution, empty_plan("test"))
            .expect("reserved plan");
        let worker = std::thread::spawn({
            let server = Arc::clone(&server);
            move || server.import_robot(execution, "ROBOT".to_owned(), "Robot {}".to_owned())
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let transaction = loop {
            world
                .exchange(ControllerEvent::Heartbeat)
                .expect("poll import");
            if let HostDirective::Mutate(NativeMutation::ImportRobot { transaction, .. }) =
                world.directive().expect("directive")
            {
                break transaction;
            }
            assert!(std::time::Instant::now() < deadline, "import begins");
            std::thread::yield_now();
        };
        world
            .exchange(ControllerEvent::RobotImported { transaction })
            .expect("scene imported");
        assert!(matches!(
            world.directive().expect("bootstrap"),
            HostDirective::Mutate(NativeMutation::StartRobotController { ready: false, .. })
        ));
        let robot = ControllerLink::connect(server.endpoint(), ControllerRole::Robot { execution })
            .expect("robot link");
        robot
            .exchange(ControllerEvent::RobotReady {
                controller: phoxal::identity::ProducerId::try_from(
                    0x2000_0000_0000_0000_0000_0000_0000_0001,
                )
                .expect("producer"),
            })
            .expect("robot ready");
        world
            .exchange(ControllerEvent::Heartbeat)
            .expect("poll ready");
        assert!(matches!(
            world.directive().expect("ready"),
            HostDirective::Mutate(NativeMutation::StartRobotController { ready: true, .. })
        ));
        world
            .exchange(ControllerEvent::MutationCompleted {
                transaction,
                error: None,
            })
            .expect("paused import complete");
        worker
            .join()
            .expect("import worker")
            .expect("import succeeds");
        assert_eq!(server.snapshot().progress().completed_step, 0);
    }

    #[test]
    fn admitted_controller_may_be_silent_beyond_the_handshake_budget() {
        let server = HostServer::bind().expect("the loopback host binds");
        let link = ControllerLink::connect(server.endpoint(), ControllerRole::World)
            .expect("the world controller connects");
        link.exchange(ControllerEvent::WorldReady {
            time_step_ns: 12_000_000,
            mode: ObservedNativeMode::Paused,
        })
        .expect("the admitted controller reports its initial boundary");

        std::thread::sleep(HANDSHAKE_TIMEOUT + Duration::from_millis(100));

        assert!(matches!(
            server.snapshot().lifecycle(),
            crate::state::NativeWorldLifecycle::Ready { .. }
        ));
        link.exchange(ControllerEvent::Heartbeat)
            .expect("admitted idle time is not classified as link loss");
    }

    #[test]
    fn completed_controller_links_release_their_shutdown_handles() {
        let server = HostServer::bind().expect("the loopback host binds");
        let baseline = lock(&server.connections).len();

        for _ in 0..8 {
            let address = server.endpoint().trim_start_matches("tcp://");
            let mut stream = TcpStream::connect(address).expect("raw client connects");
            write_frame(&mut stream, &HostRequest::Event(ControllerEvent::Heartbeat))
                .expect("invalid first frame is sent");
            assert!(matches!(
                read_frame::<_, HostResponse>(&mut stream).expect("rejection frame"),
                HostResponse::Rejected { .. }
            ));
            drop(stream);

            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            while lock(&server.connections).len() != baseline {
                assert!(
                    std::time::Instant::now() < deadline,
                    "completed controller retained a shutdown handle"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    #[test]
    fn actuation_retention_reports_every_evicted_record() {
        let mut retained = ActuationBuffer::default();
        let capability = CapabilityRef::new(
            ComponentInstanceId::new("drive").expect("component"),
            CapabilityId::new("motor").expect("capability"),
        );
        let timeline = TimelineId::from_raw(1).expect("timeline");
        for index in 0..(MAX_ACTUATION_RECORDS_PER_ROBOT as u64 + 7) {
            let progress = WorldProgress::at(index, 12).expect("progress");
            retained.push(ActuationEvidence {
                capability: capability.clone(),
                revision: 1,
                selected_at: RobotInstant::new(timeline, index),
                selected_from: progress,
                progress,
                instant: RobotInstant::new(timeline, index),
                offered: Vec::new(),
                selected: None,
                selection: ActuationSelection::None {
                    reason: NoActuationReason::Missing,
                },
                applied: AppliedActuation::Stop,
            });
        }
        assert_eq!(retained.records.len(), MAX_ACTUATION_RECORDS_PER_ROBOT);
        assert_eq!(retained.dropped, 7);
        assert_eq!(
            retained
                .records
                .front()
                .expect("oldest retained record")
                .progress
                .completed_step(),
            7
        );
    }

    #[test]
    fn non_hello_handshake_is_a_true_private_protocol_failure() {
        let server = HostServer::bind().expect("host binds");
        let address = server.endpoint().trim_start_matches("tcp://");
        let mut stream = TcpStream::connect(address).expect("raw client connects");
        write_frame(&mut stream, &HostRequest::Event(ControllerEvent::Heartbeat))
            .expect("invalid first frame is sent");
        assert!(matches!(
            read_frame::<_, HostResponse>(&mut stream).expect("rejection frame"),
            HostResponse::Rejected { .. }
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            if matches!(
                server.snapshot().lifecycle(),
                crate::state::NativeWorldLifecycle::Failed(
                    crate::state::NativeWorldFailure::Protocol(_)
                )
            ) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "invalid handshake was not classified"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn two_robot_retirement_and_failure_isolate_before_acknowledged_world_stop() {
        let server = HostServer::bind().expect("loopback host");
        let world = ControllerLink::connect(server.endpoint(), ControllerRole::World)
            .expect("world controller");
        world
            .exchange(ControllerEvent::WorldReady {
                time_step_ns: 12_000_000,
                mode: ObservedNativeMode::Paused,
            })
            .expect("world ready");

        let first = execution(0x1000_0000_0000_0000_0000_0000_0000_0001);
        let second = execution(0x2000_0000_0000_0000_0000_0000_0000_0002);
        server
            .reserve_robot(first, empty_plan("first"))
            .expect("first reservation");
        server
            .reserve_robot(second, empty_plan("second"))
            .expect("second reservation");
        let first_role = ControllerRole::Robot { execution: first };
        let second_role = ControllerRole::Robot { execution: second };
        let first_link =
            ControllerLink::connect(server.endpoint(), first_role).expect("first controller");
        let second_link =
            ControllerLink::connect(server.endpoint(), second_role).expect("second controller");
        first_link
            .exchange(ControllerEvent::RobotReady {
                controller: phoxal::identity::ProducerId::try_from(
                    0x3000_0000_0000_0000_0000_0000_0000_0003,
                )
                .expect("first producer"),
            })
            .expect("first ready");
        second_link
            .exchange(ControllerEvent::RobotReady {
                controller: phoxal::identity::ProducerId::try_from(
                    0x4000_0000_0000_0000_0000_0000_0000_0004,
                )
                .expect("second producer"),
            })
            .expect("second ready");

        server.retire_robot(first);
        first_link
            .exchange(ControllerEvent::Heartbeat)
            .expect("first retirement directive");
        assert!(matches!(
            first_link.directive().expect("retirement directive"),
            HostDirective::Stop { .. }
        ));
        first_link
            .exchange(ControllerEvent::RobotParked)
            .expect("first parked acknowledgement");
        assert!(server.robot_is_parked(first));
        server.release_robot(first);
        drop(first_link);
        wait_for_robot_release(&server, first);

        second_link
            .exchange(ControllerEvent::RobotSupervisorLost)
            .expect("second cooperative failure parks");
        assert!(matches!(
            server.robot_failure(second),
            Some(crate::state::NativeRobotFailure::SupervisorLost)
        ));
        assert!(matches!(
            server.snapshot().lifecycle(),
            crate::state::NativeWorldLifecycle::Ready { .. }
        ));
        server.retire_robot(second);
        second_link
            .exchange(ControllerEvent::RobotParked)
            .expect("second parked acknowledgement");
        server.release_robot(second);
        drop(second_link);
        wait_for_robot_release(&server, second);

        // Release removed both controller records. The same identities can be
        // admitted again, proving the native host retained no member tombstone.
        server
            .reserve_robot(first, empty_plan("first-retry"))
            .expect("released first reservation can be reused");
        let retry = ControllerLink::connect(server.endpoint(), first_role)
            .expect("released first controller identity can be reused");
        drop(retry);
        wait_for_robot_release(&server, first);
        server.release_robot(first);

        server.stop_world();
        world
            .exchange(ControllerEvent::Heartbeat)
            .expect("world stop directive");
        assert!(matches!(
            world.directive().expect("world stop directive"),
            HostDirective::Stop { .. }
        ));
        world
            .exchange(ControllerEvent::Stopped)
            .expect("world stopped acknowledgement");
        assert!(server.world_is_stopped());
    }
}
