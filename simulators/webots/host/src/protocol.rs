//! Bounded private coordination between the Webots session host and native controllers.
//!
//! This is not a public simulation API and it never leaves the local host.
//! Each controller publishes observations through a bounded nonblocking queue.
//! A socket worker performs the potentially blocking local I/O so Webots never waits for the host
//! or a robot participant while it owns a native transition.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use phoxal::api;
use phoxal::bus::RobotInstant;
use phoxal::identity::ExecutionId;
use phoxal::identity::ProducerId;
use phoxal::model::identity::CapabilityRef;
use phoxal::model::world::WorldProgress;
use phoxal::version::FrameworkVersion;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::plan::RobotSimulationPlan;

// Native indexed geometry for a current robot occupies several MiB. Keep the source
// bounded independently of its small protocol envelope and check it before mutation.
const MAX_ROBOT_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = MAX_ROBOT_SOURCE_BYTES + 1024;

pub(crate) fn validate_robot_import(definition: &str, source: &str) -> Result<(), LinkError> {
    let bytes = definition.len().saturating_add(source.len());
    if bytes > MAX_ROBOT_SOURCE_BYTES {
        return Err(LinkError::FrameTooLarge {
            bytes,
            maximum: MAX_ROBOT_SOURCE_BYTES,
        });
    }
    Ok(())
}
const EVENT_QUEUE_CAPACITY: usize = 64;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// The native role opening one private host connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ControllerRole {
    World,
    Robot { execution: ExecutionId },
}

/// The native Webots pacing mode observed at a completed boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ObservedNativeMode {
    Paused,
    RealTime,
    Run,
    Fast,
}

/// The only native motion states a Live host may request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NativeMotion {
    Paused,
    RealTime,
}

/// One observation of shared native progress.
///
/// The framework owns the public `WorldProgress` contract.
/// This private record is only the raw Webots observation the host validates before updating that
/// contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeProgressObservation {
    pub completed_step: u64,
    pub elapsed_ns: u64,
    pub mode: ObservedNativeMode,
}

/// Why one native controller can no longer be trusted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ControllerFault {
    Device { detail: String },
    InvalidProgress { detail: String },
    UnsupportedMode { observed: ObservedNativeMode },
    Protocol { detail: String },
}

/// One bounded typed record of command admission and native application.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActuationEvidence {
    pub capability: CapabilityRef,
    pub revision: u64,
    /// Monotonic Active boundary where command authority was selected before native entry.
    pub selected_at: RobotInstant,
    /// Last completed world boundary from which this command selection advanced.
    pub selected_from: WorldProgress,
    /// Completed world transition correlated with `instant`, typed outputs, and `StepEvent`.
    pub progress: WorldProgress,
    pub instant: RobotInstant,
    pub offered: Vec<OfferedActuation>,
    pub selected: Option<api::component::motor::Command>,
    pub selection: ActuationSelection,
    pub applied: AppliedActuation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ActuationSelection {
    SelectedNew,
    Reused,
    None { reason: NoActuationReason },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NoActuationReason {
    Missing,
    SourceAbsent,
    SourceConflict,
    Expired,
    ReceiverClosed,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OfferedActuation {
    pub producer: Option<ProducerId>,
    pub sequence: u64,
    pub command: api::component::motor::Command,
    pub decision: ActuationDecision,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ActuationDecision {
    Acquired,
    Renewed,
    WrongParticipant,
    ParticipantSource,
    ReadyStateOverflow,
    SourceAbsent,
    SourceConflict,
    StaleSequence {
        accepted: u64,
        observed: u64,
    },
    AuthorityHeld {
        owner: ProducerId,
    },
    NotOwner {
        owner: ProducerId,
        requested: ProducerId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum AppliedActuation {
    Position(f64),
    Velocity(f64),
    Torque(f64),
    Stop,
}

/// A bounded controller-to-host observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ControllerEvent {
    /// Poll the current directive while Webots is paused outside `wb_robot_step`.
    Heartbeat,
    WorldReady {
        time_step_ns: u64,
        mode: ObservedNativeMode,
    },
    WorldMode {
        mode: ObservedNativeMode,
    },
    WorldProgress(NativeProgressObservation),
    RobotReady {
        controller: ProducerId,
    },
    /// The controller observed and accepted one exact supervisor Active revision.
    RobotActive {
        revision: u64,
    },
    /// The Robot has completed all work for this boundary and is outside `wb_robot_step`.
    RobotBoundary {
        progress: WorldProgress,
        motion: NativeMotion,
    },
    RobotParked,
    RobotStopping,
    RobotSupervisorLost,
    ActuationEvidence(Vec<ActuationEvidence>),
    /// Native import exists; release its source and start the controller without physics.
    RobotImported {
        transaction: u64,
    },
    MutationCompleted {
        transaction: u64,
        error: Option<String>,
    },
    Stopped,
    Fault(ControllerFault),
}

/// One request on the local private connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HostRequest {
    Hello {
        framework: FrameworkVersion,
        role: ControllerRole,
    },
    Event(ControllerEvent),
}

/// The latest host directive each native controller follows at a transition boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum HostDirective {
    Continue { motion: NativeMotion },
    Park,
    Mutate(NativeMutation),
    Stop { reason: String },
}

/// One serialized scene mutation performed by the sole Webots supervisor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NativeMutation {
    ImportRobot {
        transaction: u64,
        execution: ExecutionId,
        definition: String,
        source: String,
    },
    StartRobotController {
        transaction: u64,
        execution: ExecutionId,
        ready: bool,
    },
    RemoveRobot {
        transaction: u64,
        definition: String,
    },
    /// Idempotent rollback after an import attempt with an uncertain native outcome.
    RollbackRobot {
        transaction: u64,
        definition: String,
    },
}

impl NativeMutation {
    #[must_use]
    pub const fn transaction(&self) -> u64 {
        match self {
            Self::ImportRobot { transaction, .. }
            | Self::StartRobotController { transaction, .. }
            | Self::RemoveRobot { transaction, .. }
            | Self::RollbackRobot { transaction, .. } => *transaction,
        }
    }
}

/// One host response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HostResponse {
    Accepted {
        directive: HostDirective,
        robot_plan: Option<RobotSimulationPlan>,
    },
    Directive(HostDirective),
    Rejected {
        reason: String,
    },
}

/// A private controller link failure.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("invalid private Webots host endpoint '{endpoint}'")]
    InvalidEndpoint { endpoint: String },
    #[error("failed to connect to the private Webots host at {endpoint}: {source}")]
    Connect {
        endpoint: String,
        #[source]
        source: std::io::Error,
    },
    #[error("private Webots host I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("private Webots host message encoding failed: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("private Webots host message decoding failed: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("private Webots host message is {bytes} bytes, exceeding the {maximum}-byte bound")]
    FrameTooLarge { bytes: usize, maximum: usize },
    #[error("private Webots host refused this controller: {reason}")]
    Rejected { reason: String },
    #[error("private Webots host returned an invalid handshake response")]
    InvalidHandshake,
    #[error("the bounded private Webots host event queue is full")]
    WouldBlock,
    #[error("the private Webots host link has stopped")]
    Closed,
    #[error("the private Webots host link failed: {detail}")]
    Failed { detail: String },
}

/// A controller-side private link.
///
/// `publish` only performs a bounded `try_send`.
/// The worker owns the socket and records the latest directive or terminal failure.
pub struct ControllerLink {
    events: Option<mpsc::SyncSender<QueuedEvent>>,
    state: Arc<Mutex<LinkState>>,
    robot_plan: Option<RobotSimulationPlan>,
    worker: Option<JoinHandle<()>>,
}

struct QueuedEvent {
    event: ControllerEvent,
    acknowledgement: Option<mpsc::SyncSender<Result<(), String>>>,
}

#[derive(Clone, Debug)]
enum LinkState {
    Active(HostDirective),
    Failed(String),
}

impl ControllerLink {
    /// Connect and complete the exact-train handshake before returning.
    pub fn connect(endpoint: &str, role: ControllerRole) -> Result<Self, LinkError> {
        let address = endpoint
            .strip_prefix("tcp://")
            .unwrap_or(endpoint)
            .to_owned();
        let mut addresses = address
            .to_socket_addrs()
            .map_err(|_| LinkError::InvalidEndpoint {
                endpoint: endpoint.to_owned(),
            })?;
        let address = addresses.next().ok_or_else(|| LinkError::InvalidEndpoint {
            endpoint: endpoint.to_owned(),
        })?;
        let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT).map_err(|source| {
            LinkError::Connect {
                endpoint: endpoint.to_owned(),
                source,
            }
        })?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;

        write_frame(
            &mut stream,
            &HostRequest::Hello {
                framework: FrameworkVersion::CURRENT,
                role,
            },
        )?;
        let (directive, robot_plan) = match read_frame::<_, HostResponse>(&mut stream)? {
            HostResponse::Accepted {
                directive,
                robot_plan,
            } => (directive, robot_plan),
            HostResponse::Rejected { reason } => return Err(LinkError::Rejected { reason }),
            HostResponse::Directive(_) => return Err(LinkError::InvalidHandshake),
        };

        let (events, receiver) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let state = Arc::new(Mutex::new(LinkState::Active(directive)));
        let worker_state = Arc::clone(&state);
        let worker = std::thread::Builder::new()
            .name("webots-host-link".to_owned())
            .spawn(move || run_worker(stream, receiver, &worker_state))?;
        Ok(Self {
            events: Some(events),
            state,
            robot_plan,
            worker: Some(worker),
        })
    }

    /// Take the host-authoritative plan delivered during a Robot handshake.
    pub fn take_robot_plan(&mut self) -> Result<RobotSimulationPlan, LinkError> {
        self.robot_plan.take().ok_or_else(|| LinkError::Rejected {
            reason: "the host supplied no authoritative RobotSimulationPlan".to_owned(),
        })
    }

    /// Publish one event without waiting for socket I/O.
    pub fn publish(&self, event: ControllerEvent) -> Result<(), LinkError> {
        self.enqueue(QueuedEvent {
            event,
            acknowledgement: None,
        })
    }

    /// Publish one boundary event and wait for its host directive response.
    ///
    /// Controllers call this only outside `wb_robot_step`. The bounded exchange prevents a
    /// stale Continue directive from admitting another transition after the host requested park.
    pub fn exchange(&self, event: ControllerEvent) -> Result<(), LinkError> {
        let (acknowledgement, received) = mpsc::sync_channel(0);
        self.enqueue(QueuedEvent {
            event,
            acknowledgement: Some(acknowledgement),
        })?;
        received
            .recv_timeout(IO_TIMEOUT)
            .map_err(|error| LinkError::Failed {
                detail: format!("timed out awaiting private host acknowledgement: {error}"),
            })?
            .map_err(|detail| LinkError::Failed { detail })
    }

    fn enqueue(&self, event: QueuedEvent) -> Result<(), LinkError> {
        self.ensure_active()?;
        match self
            .events
            .as_ref()
            .ok_or(LinkError::Closed)?
            .try_send(event)
        {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => Err(LinkError::WouldBlock),
            Err(mpsc::TrySendError::Disconnected(_)) => self.ensure_active(),
        }
    }

    /// Read the latest directive without waiting.
    pub fn directive(&self) -> Result<HostDirective, LinkError> {
        match &*lock(&self.state) {
            LinkState::Active(directive) => Ok(directive.clone()),
            LinkState::Failed(detail) => Err(LinkError::Failed {
                detail: detail.clone(),
            }),
        }
    }

    fn ensure_active(&self) -> Result<(), LinkError> {
        self.directive().map(|_| ())
    }
}

impl Drop for ControllerLink {
    fn drop(&mut self) {
        self.events.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_worker(
    mut stream: TcpStream,
    receiver: mpsc::Receiver<QueuedEvent>,
    state: &Arc<Mutex<LinkState>>,
) {
    for queued in receiver {
        let outcome = write_frame(&mut stream, &HostRequest::Event(queued.event))
            .and_then(|()| read_frame::<_, HostResponse>(&mut stream));
        match outcome {
            Ok(
                HostResponse::Directive(directive)
                | HostResponse::Accepted {
                    directive,
                    robot_plan: _,
                },
            ) => {
                *lock(state) = LinkState::Active(directive);
                if let Some(acknowledgement) = queued.acknowledgement {
                    let _ = acknowledgement.send(Ok(()));
                }
            }
            Ok(HostResponse::Rejected { reason }) => {
                *lock(state) = LinkState::Failed(reason.clone());
                if let Some(acknowledgement) = queued.acknowledgement {
                    let _ = acknowledgement.send(Err(reason));
                }
                return;
            }
            Err(error) => {
                let detail = error.to_string();
                *lock(state) = LinkState::Failed(detail.clone());
                if let Some(acknowledgement) = queued.acknowledgement {
                    let _ = acknowledgement.send(Err(detail));
                }
                return;
            }
        }
    }
}

pub(crate) fn write_frame<W: Write, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), LinkError> {
    let body = rmp_serde::to_vec_named(value)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(LinkError::FrameTooLarge {
            bytes: body.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    let length = u32::try_from(body.len()).map_err(|_| LinkError::FrameTooLarge {
        bytes: body.len(),
        maximum: MAX_FRAME_BYTES,
    })?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

pub(crate) fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, LinkError> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let bytes = u32::from_be_bytes(length) as usize;
    if bytes > MAX_FRAME_BYTES {
        return Err(LinkError::FrameTooLarge {
            bytes,
            maximum: MAX_FRAME_BYTES,
        });
    }
    let mut body = vec![0_u8; bytes];
    reader.read_exact(&mut body)?;
    Ok(rmp_serde::from_slice(&body)?)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn private_messages_round_trip_through_the_bounded_frame() {
        let request =
            HostRequest::Event(ControllerEvent::WorldProgress(NativeProgressObservation {
                completed_step: 42,
                elapsed_ns: 504_000_000,
                mode: ObservedNativeMode::RealTime,
            }));
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &request).expect("the private message encodes");
        let decoded = read_frame::<_, HostRequest>(&mut Cursor::new(bytes))
            .expect("the private message decodes");
        assert_eq!(decoded, request);
    }

    #[test]
    fn an_oversized_incoming_frame_is_refused_before_allocation() {
        let bytes = u32::try_from(MAX_FRAME_BYTES + 1)
            .expect("the test bound fits")
            .to_be_bytes();
        assert!(matches!(
            read_frame::<_, HostRequest>(&mut Cursor::new(bytes)),
            Err(LinkError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn robot_import_budget_admits_real_scene_sizes_before_any_mutation() {
        let source = " ".repeat(MAX_ROBOT_SOURCE_BYTES - 5);
        validate_robot_import("ROBOT", &source).expect("bounded Robot source");
        assert!(validate_robot_import("ROBOT_", &source).is_err());
        let response =
            HostResponse::Directive(HostDirective::Mutate(NativeMutation::ImportRobot {
                transaction: u64::MAX,
                execution: ExecutionId::try_from(0x1000_0000_0000_0000_0000_0000_0000_0001)
                    .expect("execution"),
                definition: "ROBOT".to_owned(),
                source,
            }));
        write_frame(&mut std::io::sink(), &response)
            .expect("preflight leaves room for the wire envelope");
    }

    #[test]
    fn unsupported_native_modes_stay_typed() {
        for observed in [ObservedNativeMode::Run, ObservedNativeMode::Fast] {
            let fault = ControllerFault::UnsupportedMode { observed };
            let bytes = rmp_serde::to_vec_named(&fault).expect("the fault encodes");
            assert_eq!(
                rmp_serde::from_slice::<ControllerFault>(&bytes).expect("the fault decodes"),
                fault
            );
        }
    }
}
