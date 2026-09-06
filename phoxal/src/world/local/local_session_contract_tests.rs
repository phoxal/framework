use super::*;
use crate::model::identity::WorldId;
use crate::model::world::{WorldDigest, WorldInstanceId, WorldProgress, WorldProvenance};
use crate::world::api::session::diagnostics::ObservedWorldPacing;
use crate::world::api::session::{WorldLifecycle, WorldMotion};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn compatible_patch_other_than_current() -> FrameworkVersion {
    let current = FrameworkVersion::CURRENT;
    let patch = if current.patch() == u16::MAX {
        current.patch() - 1
    } else {
        current.patch() + 1
    };
    FrameworkVersion::new(current.major(), current.minor(), patch)
}

struct TestHandler {
    bootstrap: WorldSessionBootstrap,
    state: std::sync::Mutex<WorldSessionState>,
    states: broadcast::Sender<WorldSessionState>,
    diagnostics: std::sync::Mutex<WorldSessionDiagnostics>,
    diagnostic_updates: broadcast::Sender<WorldSessionDiagnostics>,
    race_state_subscription: AtomicBool,
    race_diagnostics_subscription: AtomicBool,
    hang_attach: bool,
    control_calls: AtomicUsize,
    attach_calls: AtomicUsize,
}

impl TestHandler {
    fn new() -> Self {
        let instance = WorldInstanceId::mint();
        let world = WorldId::new("warehouse").expect("a valid world id");
        let digest = WorldDigest::parse(&"00".repeat(32)).expect("a canonical digest");
        let bootstrap = WorldSessionBootstrap {
            instance,
            framework: FrameworkVersion::CURRENT,
            world: world.clone(),
            digest,
        };
        let state = WorldSessionState {
            revision: 0,
            instance,
            provenance: WorldProvenance {
                world,
                digest,
                random_seed: 0,
                framework: FrameworkVersion::CURRENT,
                adapter: "test".to_owned(),
                adapter_version: "1".to_owned(),
                simulator_version: "1".to_owned(),
                platform: "test".to_owned(),
                time_step_ns: 12,
            },
            lifecycle: WorldLifecycle::Ready {
                motion: WorldMotion::Paused,
            },
            progress: WorldProgress::zero(12).expect("valid zero progress"),
            members: Vec::new(),
        };
        let (states, _) = broadcast::channel(8);
        let diagnostics = WorldSessionDiagnostics {
            revision: 0,
            pacing: None,
            last_transition_age_ns: None,
        };
        let (diagnostic_updates, _) = broadcast::channel(8);
        Self {
            bootstrap,
            state: std::sync::Mutex::new(state),
            states,
            diagnostics: std::sync::Mutex::new(diagnostics),
            diagnostic_updates,
            race_state_subscription: AtomicBool::new(false),
            race_diagnostics_subscription: AtomicBool::new(false),
            hang_attach: false,
            control_calls: AtomicUsize::new(0),
            attach_calls: AtomicUsize::new(0),
        }
    }

    fn with_subscription_races(mut self) -> Self {
        self.race_state_subscription = AtomicBool::new(true);
        self.race_diagnostics_subscription = AtomicBool::new(true);
        self
    }

    fn with_hanging_attach(mut self) -> Self {
        self.hang_attach = true;
        self
    }

    fn replace_motion(&self, motion: WorldMotion) -> WorldSessionState {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.lifecycle != (WorldLifecycle::Ready { motion }) {
            state.revision += 1;
            state.lifecycle = WorldLifecycle::Ready { motion };
            let _ = self.states.send(state.clone());
        }
        state.clone()
    }

    fn replace_diagnostics(&self, pacing: Option<ObservedWorldPacing>) -> WorldSessionDiagnostics {
        let mut diagnostics = self
            .diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        diagnostics.revision += 1;
        diagnostics.pacing = pacing;
        diagnostics.last_transition_age_ns = Some(diagnostics.revision);
        let _ = self.diagnostic_updates.send(*diagnostics);
        *diagnostics
    }
}

impl WorldSessionHandler for TestHandler {
    fn bootstrap(&self) -> WorldSessionBootstrap {
        self.bootstrap.clone()
    }

    fn state(&self) -> WorldSessionState {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn subscribe_state(&self) -> broadcast::Receiver<WorldSessionState> {
        let updates = self.states.subscribe();
        if self.race_state_subscription.swap(false, Ordering::AcqRel) {
            self.replace_motion(WorldMotion::Running);
        }
        updates
    }

    fn diagnostics(&self) -> WorldSessionDiagnostics {
        *self
            .diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn subscribe_diagnostics(&self) -> broadcast::Receiver<WorldSessionDiagnostics> {
        let updates = self.diagnostic_updates.subscribe();
        if self
            .race_diagnostics_subscription
            .swap(false, Ordering::AcqRel)
        {
            self.replace_diagnostics(None);
        }
        updates
    }

    fn control(&self, request: WorldControl) -> WorldSessionOperation<'_, WorldSessionState> {
        Box::pin(async move {
            self.control_calls.fetch_add(1, Ordering::AcqRel);
            Ok(match request {
                WorldControl::Pause => self.replace_motion(WorldMotion::Paused),
                WorldControl::Resume => self.replace_motion(WorldMotion::Running),
                WorldControl::Stop => {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.lifecycle != WorldLifecycle::Stopping {
                        state.revision += 1;
                        state.lifecycle = WorldLifecycle::Stopping;
                        let _ = self.states.send(state.clone());
                    }
                    state.clone()
                }
            })
        })
    }

    fn attach(
        &self,
        _execution: ExecutionId,
        _supervisor_endpoint: String,
        _spawn: Option<SpawnId>,
    ) -> WorldSessionOperation<'_, WorldSessionState> {
        Box::pin(async move {
            self.attach_calls.fetch_add(1, Ordering::AcqRel);
            if self.hang_attach {
                std::future::pending::<()>().await;
            }
            Ok(self.state())
        })
    }
}

#[tokio::test]
async fn loopback_client_reconciles_and_drives_idempotent_operations() {
    let handler = Arc::new(TestHandler::new());
    let server = WorldSessionServer::bind(Arc::clone(&handler))
        .await
        .expect("the loopback server binds");
    let client = WorldSessionClient::connect(server.endpoint())
        .await
        .expect("the client verifies bootstrap");
    assert_eq!(client.bootstrap(), &handler.bootstrap);

    let mut states = client
        .state_subscription()
        .await
        .expect("subscribe-first state reconciliation succeeds");
    assert_eq!(states.current().revision, 0);
    let running = client
        .control(WorldControl::Resume)
        .await
        .expect("resume is accepted");
    assert_eq!(running.revision, 1);
    assert_eq!(
        states.recv().await.expect("the replacement is delivered"),
        &running
    );
    let retry = client
        .control(WorldControl::Resume)
        .await
        .expect("resume retry is idempotent");
    assert_eq!(retry.revision, running.revision);

    let attached = client
        .attach(ExecutionId::mint(), "tcp/localhost:7447", None)
        .await
        .expect("the async host operation returns one complete state");
    assert_eq!(attached.revision, running.revision);
    assert_eq!(
        client
            .current_diagnostics()
            .await
            .expect("diagnostics current is available")
            .revision,
        0
    );

    drop(states);
    server.close().await.expect("the server closes cleanly");
}

#[tokio::test]
async fn client_rejects_state_that_contradicts_frozen_bootstrap() {
    let handler = Arc::new(TestHandler::new());
    let server = WorldSessionServer::bind(Arc::clone(&handler))
        .await
        .expect("the loopback server binds");
    let client = WorldSessionClient::connect(server.endpoint())
        .await
        .expect("the client verifies bootstrap");
    handler
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .instance = WorldInstanceId::mint();

    assert!(matches!(
        client.current_state().await,
        Err(WorldSessionWireError::BootstrapMismatch { field: "instance" })
    ));
    server.close().await.expect("the server closes cleanly");
}

#[tokio::test]
async fn attachment_preserves_the_frozen_instance_and_exact_framework_patch() {
    let handler = Arc::new(TestHandler::new());
    let server = WorldSessionServer::bind(Arc::clone(&handler))
        .await
        .expect("the loopback server binds");
    let client = WorldSessionClient::connect(server.endpoint())
        .await
        .expect("the client verifies bootstrap");
    let original_instance = handler.bootstrap.instance;
    {
        let mut state = handler
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.instance = WorldInstanceId::mint();
    }
    assert!(matches!(
        client
            .attach(ExecutionId::mint(), "tcp/localhost:7447", None)
            .await,
        Err(WorldSessionWireError::BootstrapMismatch { field: "instance" })
    ));

    let other_patch = compatible_patch_other_than_current();
    assert!(other_patch.is_compatible_with(FrameworkVersion::CURRENT));
    {
        let mut state = handler
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.instance = original_instance;
        state.provenance.framework = other_patch;
    }
    assert!(matches!(
        client
            .attach(ExecutionId::mint(), "tcp/localhost:7447", None)
            .await,
        Err(WorldSessionWireError::BootstrapMismatch { field: "framework" })
    ));

    server.close().await.expect("the server closes cleanly");
}

#[tokio::test]
async fn stale_client_cannot_mutate_a_reused_endpoint() {
    let first = Arc::new(TestHandler::new());
    let first_server = WorldSessionServer::bind(Arc::clone(&first))
        .await
        .expect("the first loopback server binds");
    let client = WorldSessionClient::connect(first_server.endpoint())
        .await
        .expect("the client captures the first bootstrap");
    let endpoint = parse_endpoint(first_server.endpoint()).expect("the endpoint parses");
    first_server
        .close()
        .await
        .expect("the first server releases its endpoint");

    let replacement = Arc::new(TestHandler::new());
    let replacement_server = WorldSessionServer::bind_at(endpoint, Arc::clone(&replacement))
        .await
        .expect("the replacement server reuses the endpoint");

    assert!(matches!(
        client.control(WorldControl::Stop).await,
        Err(WorldSessionWireError::Refused(message)) if message.contains("targets instance")
    ));
    assert_eq!(replacement.control_calls.load(Ordering::Acquire), 0);
    assert!(matches!(
        client
            .attach(ExecutionId::mint(), "tcp/localhost:7447", None)
            .await,
        Err(WorldSessionWireError::Refused(message)) if message.contains("targets instance")
    ));
    assert_eq!(replacement.attach_calls.load(Ordering::Acquire), 0);

    replacement_server
        .close()
        .await
        .expect("the replacement server closes cleanly");
}

#[tokio::test]
async fn streams_discard_subscribe_current_duplicates_and_remain_live() {
    let handler = Arc::new(TestHandler::new().with_subscription_races());
    let server = WorldSessionServer::bind(Arc::clone(&handler))
        .await
        .expect("the loopback server binds");
    let client = WorldSessionClient::connect(server.endpoint())
        .await
        .expect("the client verifies bootstrap");

    let mut states = client
        .state_subscription()
        .await
        .expect("the raced state snapshot reconciles");
    assert_eq!(states.current().revision, 1);
    let paused = handler.replace_motion(WorldMotion::Paused);
    assert_eq!(
        states.recv().await.expect("the state stream remains live"),
        &paused
    );

    let mut diagnostics = client
        .diagnostics_subscription()
        .await
        .expect("the raced diagnostics snapshot reconciles");
    assert_eq!(diagnostics.current().revision, 1);
    let next = handler.replace_diagnostics(Some(ObservedWorldPacing {
        world_elapsed_ns: 12,
        host_elapsed_ns: 20,
        completed_transitions: 1,
    }));
    assert_eq!(
        diagnostics
            .recv()
            .await
            .expect("the diagnostics stream remains live"),
        next
    );

    server.close().await.expect("the server closes cleanly");
}

#[tokio::test]
async fn invalid_pacing_is_rejected_from_current_and_streamed_diagnostics() {
    let handler = Arc::new(TestHandler::new());
    let server = WorldSessionServer::bind(Arc::clone(&handler))
        .await
        .expect("the loopback server binds");
    let client = WorldSessionClient::connect(server.endpoint())
        .await
        .expect("the client verifies bootstrap");

    handler.replace_diagnostics(Some(ObservedWorldPacing {
        world_elapsed_ns: 0,
        host_elapsed_ns: 1,
        completed_transitions: 1,
    }));
    assert!(matches!(
        client.current_diagnostics().await,
        Err(WorldSessionWireError::Diagnostics(_))
    ));

    handler.replace_diagnostics(None);
    let mut diagnostics = client
        .diagnostics_subscription()
        .await
        .expect("valid diagnostics subscribe");
    handler.replace_diagnostics(Some(ObservedWorldPacing {
        world_elapsed_ns: 1,
        host_elapsed_ns: 0,
        completed_transitions: 1,
    }));
    assert!(matches!(
        diagnostics.recv().await,
        Err(WorldSessionWireError::Diagnostics(_))
    ));

    server.close().await.expect("the server closes cleanly");
}

#[tokio::test]
async fn client_and_host_operations_have_typed_deadlines() {
    let silent_listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("the silent listener binds");
    let silent_address = silent_listener
        .local_addr()
        .expect("an address is assigned");
    let silent = tokio::spawn(async move {
        let (_stream, _) = silent_listener.accept().await.expect("a client connects");
        std::future::pending::<()>().await;
    });
    let error = WorldSessionClient::connect(&format!("tcp://{silent_address}"))
        .await
        .expect_err("a silent listener must time out");
    assert!(matches!(
        error,
        WorldSessionWireError::Timeout { ref operation, .. }
            if operation == "request response"
    ));
    silent.abort();

    let handler = Arc::new(TestHandler::new().with_hanging_attach());
    let server = WorldSessionServer::bind(Arc::clone(&handler))
        .await
        .expect("the loopback server binds");
    let client = WorldSessionClient::connect(server.endpoint())
        .await
        .expect("the client verifies bootstrap");
    let error = client
        .attach(ExecutionId::mint(), "tcp/localhost:7447", None)
        .await
        .expect_err("a hung host operation must time out");
    assert!(matches!(
        error,
        WorldSessionWireError::Timeout { ref operation, .. }
            if operation == "host attachment"
    ));
    server.close().await.expect("the server closes cleanly");
}

#[tokio::test]
async fn idle_handshakes_release_the_bounded_connection_permits() {
    let handler = Arc::new(TestHandler::new());
    let server = WorldSessionServer::bind(Arc::clone(&handler))
        .await
        .expect("the loopback server binds");
    let endpoint = parse_endpoint(server.endpoint()).expect("the endpoint parses");
    let mut idle = Vec::with_capacity(MAX_CONNECTIONS);
    for _ in 0..MAX_CONNECTIONS {
        idle.push(
            TcpStream::connect(endpoint)
                .await
                .expect("an idle client connects"),
        );
    }
    tokio::time::sleep(HANDSHAKE_TIMEOUT + Duration::from_millis(100)).await;

    WorldSessionClient::connect(server.endpoint())
        .await
        .expect("expired handshakes release permits for a valid client");
    drop(idle);
    server.close().await.expect("the server closes cleanly");
}

#[tokio::test]
async fn idle_subscription_disconnects_release_connection_permits() {
    let handler = Arc::new(TestHandler::new());
    let server = WorldSessionServer::bind(Arc::clone(&handler))
        .await
        .expect("the loopback server binds");
    let client = WorldSessionClient::connect(server.endpoint())
        .await
        .expect("the client verifies bootstrap");

    for _ in 0..MAX_CONNECTIONS * 2 {
        let subscription = client
            .state_subscription()
            .await
            .expect("an idle state subscription opens");
        drop(subscription);
        tokio::task::yield_now().await;
    }

    tokio::time::timeout(Duration::from_secs(2), client.control(WorldControl::Resume))
        .await
        .expect("idle subscriptions release their server permits")
        .expect("a fresh control request succeeds");
    server.close().await.expect("the server closes cleanly");
}
