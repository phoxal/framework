#![cfg(feature = "runtime")]
#![allow(clippy::expect_used, reason = "test fixture setup and assertions")]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use phoxal::contract::MethodDescriptor;
use phoxal::identity::ExecutionId;
use phoxal::runtime::connection::{Connection, ConnectionConfig, ConnectionOwner};
use phoxal::runtime::execution_protocol::{self, wire as execution_wire};
use phoxal::runtime::transport::{self, RuntimeWireMetadata, WireSample};
use phoxal::runtime::{ExecutionTime, ObservationStamp};
use phoxal_service_kinematics::{OdometryState, kinematics};
use phoxal_service_world::{Bounds, WindowRequest, WindowResponse, window_response, world};
use prost::Message;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::process::{Child, Command};
use zenoh::bytes::Encoding;

type Subscriber =
    zenoh::pubsub::Subscriber<zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>>;

fn reserve_tcp_endpoint() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve router port");
    let port = listener.local_addr().expect("router address").port();
    drop(listener);
    format!("tcp/127.0.0.1:{port}")
}

async fn open_router(endpoint: &str) -> zenoh::Session {
    let mut config = zenoh::Config::default();
    phoxal::runtime::connection::apply_phoxal_transport_policy(&mut config)
        .expect("transport policy");
    config
        .insert_json5("mode", "\"router\"")
        .expect("router mode");
    config
        .insert_json5(
            "listen/endpoints",
            &serde_json::to_string(&[endpoint]).expect("router endpoint JSON"),
        )
        .expect("router endpoint");
    zenoh::open(config).await.expect("router opens")
}

fn install_bundle(binary: &Path) -> (tempfile::TempDir, PathBuf, Vec<u8>) {
    let root = tempfile::tempdir().expect("bundle directory");
    let installed = root.path().join("phoxal-service-world");
    std::fs::copy(binary, &installed).expect("copy world binary");
    let bytes = std::fs::read(&installed).expect("read world binary");
    let digest = Sha256::digest(&bytes);
    let signature = kinematics::methods::ODOMETRY.signature();
    let manifest = json!({
        "schema": "phoxal/bundle/v0",
        "robot_id": "world-read-transport-proof",
        "document": {
            "services": { "world": { "config": {} } },
            "connections": { "world.pose": "kinematics.odometry" }
        },
        "executables": [{
            "instance": "world",
            "path": "phoxal-service-world",
            "bytes": bytes.len(),
            "sha256": format!("{digest:x}")
        }],
        "simulation": {
            "providers": [{
                "service_instance": "kinematics",
                "port": signature.endpoint,
                "service_fqn": signature.service,
                "method": signature.method,
                "shape": "observation",
                "retained_latest": signature.retained_latest,
                "input_fqn": signature.request,
                "payload_fqn": signature.response,
                "max_message_bytes": 512,
                "max_buffered_items": 2
            }]
        }
    });
    std::fs::write(
        root.path().join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("manifest encodes"),
    )
    .expect("manifest writes");
    (root, installed, digest.to_vec())
}

async fn subscribe(bus: &Connection, instance: &str, leg: &str) -> Subscriber {
    bus.session()
        .expect("session is open")
        .declare_subscriber(execution_protocol::key(bus, instance, leg))
        .with(zenoh::handlers::FifoChannel::new(8))
        .await
        .expect("execution subscriber")
}

async fn publish_execution<M: Message>(bus: &Connection, leg: &str, message: &M) {
    bus.session()
        .expect("session is open")
        .put(
            execution_protocol::key(bus, "world", leg),
            execution_protocol::encode(message).expect("execution message encodes"),
        )
        .encoding(Encoding::from(
            execution_protocol::PROTOBUF_ENCODING.to_owned(),
        ))
        .await
        .expect("execution message publishes");
}

async fn admit_world(
    bus: &Connection,
    responses: &Subscriber,
    execution: ExecutionId,
    digest: Vec<u8>,
) {
    let request = execution_wire::AdmitExecutionRequest {
        execution_id: execution.to_string(),
        timeline_id: "hardware".to_owned(),
        artifact_digest: digest,
        required_contracts: vec![execution_wire::ContractRequirement {
            protocol: "phoxal.execution.v1".to_owned(),
            capabilities: execution_protocol::REQUIRED_CAPABILITIES
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        }],
        mode: execution_wire::ExecutionMode::Hardware as i32,
        quantum_ns: 0,
    };
    for _ in 0..40 {
        publish_execution(bus, "admit", &request).await;
        if let Ok(Ok(sample)) =
            tokio::time::timeout(Duration::from_millis(50), responses.recv_async()).await
        {
            let response: execution_wire::AdmitExecutionResponse =
                execution_protocol::decode(sample.payload().to_bytes().as_ref())
                    .expect("admission response decodes");
            assert!(
                response.admitted,
                "admission refused: {:?}",
                response.detail
            );
            return;
        }
    }
    panic!("world runtime did not answer admission");
}

async fn publish_odometry(bus: &Connection, sequence: u64, elapsed: Duration) {
    let capture = ExecutionTime::from(elapsed);
    let value = OdometryState {
        x_m: 0.2,
        y_m: 0.2,
        yaw_rad: 0.0,
        linear_x_mps: 0.0,
        angular_z_radps: 0.0,
        revision: sequence,
        available: true,
        oldest_capture_time_nanos: Some(capture.as_nanos()),
    };
    let metadata = RuntimeWireMetadata::observed(
        &ObservationStamp::new("kinematics", capture, Some(sequence)),
        sequence,
    )
    .encode_bounded()
    .expect("odometry metadata encodes");
    bus.session()
        .expect("session is open")
        .put(
            bus.full_key(&transport::port_key(
                "kinematics",
                kinematics::methods::ODOMETRY.signature().endpoint,
                "publish",
            )),
            transport::encode_prost(&value).expect("odometry encodes"),
        )
        .encoding(Encoding::from(transport::PROTOBUF_ENCODING.to_owned()))
        .attachment(metadata)
        .await
        .expect("odometry publishes");
}

async fn query_window(bus: &Connection, replies: &Subscriber, command_id: u64) -> WindowResponse {
    let request = WindowRequest {
        requested: Some(Bounds {
            min_x_m: 0.1,
            min_y_m: 0.1,
            max_x_m: 0.9,
            max_y_m: 0.9,
        }),
        revision: 0,
    };
    let metadata =
        RuntimeWireMetadata::external_request(ExecutionTime::default(), command_id, 0, command_id);
    bus.session()
        .expect("session is open")
        .put(
            bus.full_key(&transport::port_key(
                "world",
                world::methods::WINDOW.signature().endpoint,
                "request",
            )),
            transport::encode_prost(&request).expect("window request encodes"),
        )
        .encoding(Encoding::from(transport::PROTOBUF_ENCODING.to_owned()))
        .attachment(metadata.encode_bounded().expect("request metadata encodes"))
        .await
        .expect("window request publishes");
    let sample = tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
        .await
        .expect("window reply deadline")
        .expect("window reply");
    let wire = WireSample::from_zenoh(sample).expect("window reply metadata");
    WindowResponse::decode(wire.payload()).expect("window response decodes")
}

async fn stop_child(child: &mut Child) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_world_window_call_runs_while_odometry_keeps_its_own_schedule() {
    let endpoint = reserve_tcp_endpoint();
    let router = open_router(&endpoint).await;
    let execution = ExecutionId::mint();
    let (owner, bus) = ConnectionOwner::open(ConnectionConfig::for_external(
        execution,
        Some("world-read-proof".to_owned()),
        vec![endpoint.clone()],
    ))
    .await
    .expect("supervisor bus opens");
    let admission_responses = subscribe(&bus, "world", "admit-response").await;
    let ready = subscribe(&bus, "world", "ready").await;
    let replies = bus
        .session()
        .expect("session is open")
        .declare_subscriber(bus.full_key(&transport::port_key(
            "world",
            world::methods::WINDOW.signature().endpoint,
            "reply",
        )))
        .with(zenoh::handlers::FifoChannel::new(8))
        .await
        .expect("window reply subscriber");
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_phoxal-service-world"));
    let (bundle, installed, digest) = install_bundle(&binary);
    let mut child = Command::new(installed)
        .arg("--bundle-root")
        .arg(bundle.path())
        .arg("--instance-id")
        .arg("world")
        .arg("--execution-id")
        .arg(execution.to_string())
        .arg("--connect")
        .arg(&endpoint)
        .kill_on_drop(true)
        .spawn()
        .expect("world runtime starts");

    admit_world(&bus, &admission_responses, execution, digest).await;
    tokio::time::timeout(Duration::from_secs(2), ready.recv_async())
        .await
        .expect("world ready deadline")
        .expect("world ready");

    let running = Arc::new(AtomicBool::new(true));
    let provider_running = Arc::clone(&running);
    let provider_bus = bus.clone();
    let provider = tokio::spawn(async move {
        let origin = Instant::now();
        let mut sequence = 1;
        while provider_running.load(Ordering::Acquire) {
            publish_odometry(&provider_bus, sequence, origin.elapsed()).await;
            sequence += 1;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        sequence
    });

    let mut available = None;
    for command_id in 1..=20 {
        let response = query_window(&bus, &replies, command_id).await;
        if matches!(response.result, Some(window_response::Result::Window(_))) {
            available = Some((command_id, response));
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (completed_command_id, response) =
        available.expect("World did not expose an available window after fresh odometry");
    let Some(window_response::Result::Window(window)) = response.result.as_ref() else {
        unreachable!("loop exits only with an available window")
    };
    assert!(window.revision > 0);
    assert_eq!(window.frame_id, "odom");

    tokio::time::sleep(Duration::from_millis(30)).await;
    let later = query_window(&bus, &replies, completed_command_id + 100).await;
    let Some(window_response::Result::Window(later_window)) = later.result else {
        panic!("a later World call must return the current available window")
    };
    assert!(later_window.revision >= window.revision);
    running.store(false, Ordering::Release);
    assert!(provider.await.expect("provider joins") > 2);

    stop_child(&mut child).await;
    let close = owner.close().await;
    assert!(close.worker_errors.is_empty());
    router.close().await.expect("router closes");
}
