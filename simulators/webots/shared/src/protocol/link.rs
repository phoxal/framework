use super::*;

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

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
