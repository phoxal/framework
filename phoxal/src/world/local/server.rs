use super::*;

/// One host operation whose completion is driven asynchronously by the host.
///
/// Attachment may perform supervisor queries, controller readiness
/// coordination, native simulator mutation, and rollback. Keeping that work
/// asynchronous prevents a client connection from blocking the Tokio worker
/// that serves the local session endpoint.
pub type WorldSessionOperation<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// Host-owned state and operation hooks served by [`WorldSessionServer`].
///
/// Implementations must make `state` and `subscribe_state` one serialized
/// authority, and likewise for diagnostics. The server subscribes before it
/// reads current, then filters buffered revisions, closing both races.
pub trait WorldSessionHandler: Send + Sync + 'static {
    fn bootstrap(&self) -> WorldSessionBootstrap;
    fn state(&self) -> WorldSessionState;
    fn subscribe_state(&self) -> broadcast::Receiver<WorldSessionState>;
    fn diagnostics(&self) -> WorldSessionDiagnostics;
    fn subscribe_diagnostics(&self) -> broadcast::Receiver<WorldSessionDiagnostics>;
    fn control(&self, operation: WorldControl) -> WorldSessionOperation<'_, WorldSessionState>;
    fn attach(
        &self,
        execution: ExecutionId,
        supervisor_endpoint: String,
        spawn: Option<SpawnId>,
    ) -> WorldSessionOperation<'_, WorldSessionState>;
}

/// The unique listener for one local world session.
pub struct WorldSessionServer {
    endpoint: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), WorldSessionWireError>>>,
}

impl WorldSessionServer {
    /// Bind a private loopback port and start serving one host authority.
    pub async fn bind<H: WorldSessionHandler>(
        handler: Arc<H>,
    ) -> Result<Self, WorldSessionWireError> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        Self::from_listener(listener, handler).await
    }

    #[cfg(test)]
    pub(super) async fn bind_at<H: WorldSessionHandler>(
        address: SocketAddr,
        handler: Arc<H>,
    ) -> Result<Self, WorldSessionWireError> {
        let listener = TcpListener::bind(address).await?;
        Self::from_listener(listener, handler).await
    }

    async fn from_listener<H: WorldSessionHandler>(
        listener: TcpListener,
        handler: Arc<H>,
    ) -> Result<Self, WorldSessionWireError> {
        let address = listener.local_addr()?;
        let endpoint = format!("tcp://{address}");
        let (shutdown, stop) = oneshot::channel();
        let task = tokio::spawn(serve(listener, handler, stop));
        Ok(Self {
            endpoint,
            shutdown: Some(shutdown),
            task: Some(task),
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub async fn close(mut self) -> Result<(), WorldSessionWireError> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        task.await
            .map_err(|error| WorldSessionWireError::Protocol(error.to_string()))?
    }
}

impl Drop for WorldSessionServer {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn serve<H: WorldSessionHandler>(
    listener: TcpListener,
    handler: Arc<H>,
    mut shutdown: oneshot::Receiver<()>,
) -> Result<(), WorldSessionWireError> {
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                connections.shutdown().await;
                return Ok(());
            }
            accepted = listener.accept() => {
                let (stream, address) = accepted?;
                if !address.ip().is_loopback() {
                    continue;
                }
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    continue;
                };
                let handler = Arc::clone(&handler);
                connections.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = serve_connection(stream, handler).await {
                        tracing::warn!(target: "phoxal.world.session", %error, "world-session client ended with an error");
                    }
                });
            }
            Some(joined) = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = joined {
                    tracing::warn!(target: "phoxal.world.session", %error, "world-session connection task failed");
                }
            }
        }
    }
}

async fn serve_connection<H: WorldSessionHandler>(
    mut stream: TcpStream,
    handler: Arc<H>,
) -> Result<(), WorldSessionWireError> {
    let request: WireRequest = with_timeout(
        "server handshake",
        HANDSHAKE_TIMEOUT,
        read_frame(&mut stream),
    )
    .await?;
    match request.path.as_str() {
        STATE_PATH => {
            let request = decode_body::<WorldSessionStateSubscriptionRequest>(&request.body)?;
            if let Err(error) = validate_instance(handler.as_ref(), request.instance) {
                return send_error(&mut stream, error.to_string()).await;
            }
            serve_state_stream(&mut stream, handler).await
        }
        STATE_CURRENT_PATH => {
            let request = decode_body::<WorldSessionStateCurrentRequest>(&request.body)?;
            if let Err(error) = validate_instance(handler.as_ref(), request.instance) {
                return send_error(&mut stream, error.to_string()).await;
            }
            send_value(
                &mut stream,
                &WorldSessionStateCurrentResponse {
                    state: handler.state(),
                },
            )
            .await
        }
        DIAGNOSTICS_PATH => {
            let request = decode_body::<WorldSessionDiagnosticsSubscriptionRequest>(&request.body)?;
            if let Err(error) = validate_instance(handler.as_ref(), request.instance) {
                return send_error(&mut stream, error.to_string()).await;
            }
            serve_diagnostics_stream(&mut stream, handler).await
        }
        DIAGNOSTICS_CURRENT_PATH => {
            let request = decode_body::<WorldSessionDiagnosticsCurrentRequest>(&request.body)?;
            if let Err(error) = validate_instance(handler.as_ref(), request.instance) {
                return send_error(&mut stream, error.to_string()).await;
            }
            send_value(
                &mut stream,
                &WorldSessionDiagnosticsCurrentResponse {
                    diagnostics: handler.diagnostics(),
                },
            )
            .await
        }
        CONTROL_PATH => {
            let control = decode_body::<WorldSessionControlRequest>(&request.body)?;
            if let Err(error) = validate_instance(handler.as_ref(), control.instance) {
                return send_error(&mut stream, error.to_string()).await;
            }
            match tokio::time::timeout(HOST_OPERATION_TIMEOUT, handler.control(control.operation))
                .await
            {
                Ok(Ok(state)) => {
                    send_value(&mut stream, &WorldSessionControlResponse { state }).await
                }
                Ok(Err(message)) => send_error(&mut stream, message).await,
                Err(_) => send_timeout(&mut stream, "host control", HOST_OPERATION_TIMEOUT).await,
            }
        }
        CONNECT_PATH => serve_connect(&mut stream, handler, &request.body).await,
        _ => {
            send_error(
                &mut stream,
                format!("unknown world-session path '{}'", request.path),
            )
            .await
        }
    }
}

async fn serve_connect<H: WorldSessionHandler>(
    stream: &mut TcpStream,
    handler: Arc<H>,
    body: &[u8],
) -> Result<(), WorldSessionWireError> {
    match decode_body::<WorldSessionConnectRequest>(body)? {
        WorldSessionConnectRequest::Bootstrap { .. } => {
            send_value(
                stream,
                &WorldSessionConnectResponse::Bootstrap {
                    bootstrap: handler.bootstrap(),
                },
            )
            .await
        }
        WorldSessionConnectRequest::Attach {
            framework,
            instance,
            execution,
            supervisor_endpoint,
            spawn,
        } => {
            if !framework.is_compatible_with(FrameworkVersion::CURRENT) {
                return send_error(
                    stream,
                    format!(
                        "framework {framework} is incompatible with host {}",
                        FrameworkVersion::CURRENT
                    ),
                )
                .await;
            }
            if let Err(error) = validate_instance(handler.as_ref(), instance) {
                return send_error(stream, error.to_string()).await;
            }
            match tokio::time::timeout(
                HOST_OPERATION_TIMEOUT,
                handler.attach(execution, supervisor_endpoint, spawn),
            )
            .await
            {
                Ok(Ok(state)) => {
                    send_value(stream, &WorldSessionConnectResponse::Attached { state }).await
                }
                Ok(Err(message)) => send_error(stream, message).await,
                Err(_) => send_timeout(stream, "host attachment", HOST_OPERATION_TIMEOUT).await,
            }
        }
    }
}

fn validate_instance<H: WorldSessionHandler>(
    handler: &H,
    requested: crate::model::world::WorldInstanceId,
) -> Result<(), WorldSessionWireError> {
    let actual = handler.bootstrap().instance;
    if requested == actual {
        Ok(())
    } else {
        Err(WorldSessionWireError::Protocol(format!(
            "world-session request targets instance {requested}, but this endpoint serves {actual}"
        )))
    }
}

async fn serve_state_stream<H: WorldSessionHandler>(
    stream: &mut TcpStream,
    handler: Arc<H>,
) -> Result<(), WorldSessionWireError> {
    let (mut peer, mut output) = stream.split();
    let mut updates = handler.subscribe_state();
    let current = handler.state();
    let mut revision = current.revision;
    send_value(&mut output, &WorldSessionStateStream { state: current }).await?;
    loop {
        match updates.try_recv() {
            Ok(state) if state.revision > revision => {
                revision = state.revision;
                send_value(&mut output, &WorldSessionStateStream { state }).await?;
            }
            Ok(_) => continue,
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                send_gap(&mut output, "state", skipped).await?;
                return Ok(());
            }
            Err(broadcast::error::TryRecvError::Closed) => return Ok(()),
        }
    }
    serve_subscription(
        &mut peer,
        &mut output,
        updates,
        revision,
        "state",
        |state| WorldSessionStateStream { state },
    )
    .await
}

async fn serve_diagnostics_stream<H: WorldSessionHandler>(
    stream: &mut TcpStream,
    handler: Arc<H>,
) -> Result<(), WorldSessionWireError> {
    let (mut peer, mut output) = stream.split();
    let mut updates = handler.subscribe_diagnostics();
    let current = handler.diagnostics();
    let mut revision = current.revision;
    send_value(
        &mut output,
        &WorldSessionDiagnosticsStream {
            diagnostics: current,
        },
    )
    .await?;
    loop {
        match updates.try_recv() {
            Ok(diagnostics) if diagnostics.revision > revision => {
                revision = diagnostics.revision;
                send_value(&mut output, &WorldSessionDiagnosticsStream { diagnostics }).await?;
            }
            Ok(_) => continue,
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                send_gap(&mut output, "diagnostics", skipped).await?;
                return Ok(());
            }
            Err(broadcast::error::TryRecvError::Closed) => return Ok(()),
        }
    }
    serve_subscription(
        &mut peer,
        &mut output,
        updates,
        revision,
        "diagnostics",
        |diagnostics| WorldSessionDiagnosticsStream { diagnostics },
    )
    .await
}

async fn serve_subscription<T, U>(
    peer: &mut tokio::net::tcp::ReadHalf<'_>,
    output: &mut tokio::net::tcp::WriteHalf<'_>,
    mut updates: broadcast::Receiver<T>,
    mut revision: u64,
    stream_name: &'static str,
    wrap: impl Fn(T) -> U,
) -> Result<(), WorldSessionWireError>
where
    T: Clone + Revisioned,
    U: Serialize,
{
    let mut peer_byte = [0_u8; 1];
    loop {
        tokio::select! {
            update = updates.recv() => match update {
                Ok(update) if update.revision() > revision => {
                    revision = update.revision();
                    send_value(output, &wrap(update)).await?;
                }
                Ok(update) => {
                    send_error(
                        output,
                        format!(
                            "world {stream_name} revision {} did not increase beyond {revision}",
                            update.revision()
                        ),
                    )
                    .await?;
                    return Ok(());
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    send_gap(output, stream_name, skipped).await?;
                    return Ok(());
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            result = peer.read(&mut peer_byte) => match result {
                Ok(0) => return Ok(()),
                Ok(_) => return Err(WorldSessionWireError::Protocol(
                    "world-session subscription client sent unexpected data".to_owned(),
                )),
                Err(error) => return Err(error.into()),
            },
        }
    }
}

trait Revisioned {
    fn revision(&self) -> u64;
}

impl Revisioned for WorldSessionState {
    fn revision(&self) -> u64 {
        self.revision
    }
}

impl Revisioned for WorldSessionDiagnostics {
    fn revision(&self) -> u64 {
        self.revision
    }
}
