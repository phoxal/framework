use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use prost::Message;

use super::input::{CollectedInput, DeliveryAdmissionError, DeliveryQueue};
use super::*;
use crate::runtime::input::OperationInputError;
use crate::runtime::transport::{PreparedOutput, RuntimeWireMetadata};
use crate::runtime::{
    ExecutionDuration, InitContext, ObservationStamp, ReadError, RequestError, Runtime,
    RuntimeSpec, StepContext,
};

#[derive(Debug, Deserialize)]
struct TestConfig {
    value: u64,
}

impl Config for TestConfig {
    const SCHEMA_JSON: &'static str = "{}";
}

fn controlled_sample(
    source: &str,
    execution_id: &str,
    timeline_id: &str,
    boundary: u64,
    item: u32,
    bytes: usize,
) -> WireSample {
    let metadata = RuntimeWireMetadata::data(source, ExecutionTime::default(), boundary)
        .with_delivery_identity(execution_id, timeline_id, boundary, item);
    WireSample::from_parts(vec![0; bytes], metadata, "runtime/test")
}

#[test]
fn forwarding_keeps_observation_provenance_separate_from_delivery_identity() {
    let stamp = ObservationStamp::new("physical-sensor", ExecutionTime::from_nanos(17), Some(3));
    let mut metadata = transport::RuntimeWireMetadata::observed(&stamp, 4).with_delivery_identity(
        "execution",
        "timeline",
        9,
        0,
    );
    metadata.producer = Some("forwarder".into());
    let sample = WireSample::from_parts(vec![7], metadata, "test");
    let queue = DeliveryQueue::new(8, 64, super::super::input::InputKind::Samples);
    let identity = queue
        .identity(&sample, "consumer", "ranges", "publish")
        .unwrap();
    assert_eq!(identity.source, "forwarder");
    assert_eq!(sample.metadata().source.as_deref(), Some("physical-sensor"));
    assert_eq!(sample.metadata().logical_time_nanos, Some(17));
}

#[test]
fn receiver_keeps_current_value_when_next_boundary_output_arrives_early() {
    let mut queue = DeliveryQueue::new(1, 8, super::super::input::InputKind::Latest);
    for boundary in [4, 5] {
        queue
            .admit(
                controlled_sample("producer", "execution", "timeline", boundary, 0, 8),
                "consumer",
                "value",
                "publish",
            )
            .expect("bounded latest admission");
    }
    assert_eq!(
        queue.items.len(),
        2,
        "keep the current and future cut independently"
    );
    let current = queue.drain(5);
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].metadata().boundary(), 4);
    assert_eq!(queue.items.len(), 2);
    let next = queue.drain(6);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].metadata().boundary(), 5);
    assert_eq!(queue.bytes, 8);
    assert_eq!(queue.drain(7)[0].metadata().boundary(), 5);
}

#[test]
fn camera_latest_queue_has_two_bounded_cuts_and_rejects_oversize_frames() {
    let cap = 4 * 1024 * 1024;
    let mut queue = DeliveryQueue::new(1, cap, super::super::input::InputKind::Latest);
    for boundary in 0..4 {
        queue
            .admit(
                controlled_sample("camera", "execution", "timeline", boundary, 0, cap as usize),
                "viewer",
                "rgb",
                "publish",
            )
            .unwrap();
        assert!(queue.items.len() <= 2);
        assert!(queue.bytes <= 2 * cap);
    }
    let saturated = queue.admit(
        controlled_sample("camera", "execution", "timeline", 4, 0, cap as usize + 1),
        "viewer",
        "rgb",
        "publish",
    );
    assert!(matches!(
        saturated,
        Err(DeliveryAdmissionError::Saturated(_))
    ));
    assert_eq!(queue.drain(3)[0].metadata().boundary(), 2);
    assert_eq!(queue.drain(4)[0].metadata().boundary(), 3);
}

#[test]
fn same_delivery_identity_with_different_equal_length_bytes_is_rejected() {
    let mut queue = DeliveryQueue::new(2, 8, super::super::input::InputKind::Samples);
    let sample = controlled_sample("producer", "execution", "timeline", 1, 0, 1);
    queue
        .admit(sample.clone(), "consumer", "value", "publish")
        .unwrap();
    let conflicting = WireSample::from_parts(vec![1], sample.metadata().clone(), "runtime/test");
    assert!(matches!(
        queue.admit(conflicting, "consumer", "value", "publish"),
        Err(DeliveryAdmissionError::Malformed(_))
    ));
}

#[test]
fn receiver_queue_keeps_unstamped_hardware_and_supervisor_ingress() {
    let mut queue = DeliveryQueue::new(4, 64, super::super::input::InputKind::Samples);
    let hardware = WireSample::from_parts(
        vec![1, 2],
        RuntimeWireMetadata::data("sensor", ExecutionTime::default(), 1),
        "runtime/sensor/ports/value/publish",
    );
    let external = WireSample::from_parts(
        vec![3],
        RuntimeWireMetadata::external_request(ExecutionTime::default(), 7, 0, 1),
        "runtime/target/ports/commands/request",
    );
    queue.admit_untracked(hardware).expect("hardware admission");
    queue.admit_untracked(external).expect("external admission");
    assert_eq!(queue.drain(0).len(), 2);
}

#[test]
fn receiver_queue_dedupe_is_a_bounded_high_watermark() {
    let mut queue = DeliveryQueue::new(20_001, 20_001, super::super::input::InputKind::Samples);
    queue.set_timeline("timeline");
    for boundary in 0..20_000 {
        let (_, inserted) = queue
            .admit(
                controlled_sample("producer", "execution", "timeline", boundary, 0, 1),
                "consumer",
                "value",
                "publish",
            )
            .expect("controlled delivery admission");
        assert!(inserted);
    }
    assert_eq!(queue.high_watermarks.len(), 1);
    let (_, inserted) = queue
        .admit(
            controlled_sample("producer", "execution", "timeline", 19_999, 0, 1),
            "consumer",
            "value",
            "publish",
        )
        .expect("duplicate admission is idempotent");
    assert!(!inserted);
    assert_eq!(queue.items.len(), 20_000);
}

#[test]
fn receiver_queue_fan_in_uses_one_aggregate_bound() {
    let mut queue = DeliveryQueue::new(2, 2, super::super::input::InputKind::Samples);
    for (source, boundary) in [("left", 0), ("right", 0)] {
        queue
            .admit(
                controlled_sample(source, "execution", "timeline", boundary, 0, 1),
                "consumer",
                "value",
                "publish",
            )
            .expect("fan-in item fits aggregate queue");
    }
    let saturated = queue.admit(
        controlled_sample("left", "execution", "timeline", 1, 0, 1),
        "consumer",
        "value",
        "publish",
    );
    assert!(matches!(
        saturated,
        Err(DeliveryAdmissionError::Saturated(_))
    ));
}

#[test]
fn receiver_queue_rejects_stale_timeline_after_reset_fence() {
    let mut queue = DeliveryQueue::new(4, 4, super::super::input::InputKind::Samples);
    queue.set_timeline("timeline-1");
    let current = controlled_sample("producer", "execution", "timeline-1", 0, 0, 1);
    let identity = queue
        .identity(&current, "consumer", "value", "publish")
        .expect("identity decodes");
    assert!(queue.accepts_timeline(&identity.timeline_id));
    queue.set_timeline("timeline-2");
    assert!(!queue.accepts_timeline(&identity.timeline_id));
    assert!(queue.items.is_empty());
    assert!(queue.high_watermarks.is_empty());
}

#[derive(Default)]
struct TestInputs;

impl super::super::input::InputSet for TestInputs {
    const FIELDS: &'static [super::super::input::InputField] = &[];
}

impl InputSnapshot for TestInputs {
    type Transport = ();

    fn empty() -> Self {
        Self
    }
}

struct TestOutputs {
    value: u64,
}

impl super::super::outputs::OutputSet for TestOutputs {
    const FIELDS: &'static [super::super::outputs::OutputField] = &[];
}

struct TestRuntime {
    validate: Arc<Mutex<Vec<&'static str>>>,
    fail_step: bool,
    step_delay: Duration,
}

impl Runtime for TestRuntime {
    type Config = TestConfig;
    type State = u64;
    type Inputs = TestInputs;
    type Outputs = TestOutputs;

    fn validate_config(config: &Self::Config) -> crate::Result<()> {
        (config.value > 0)
            .then_some(())
            .ok_or_else(|| anyhow::anyhow!("value must be positive"))
    }

    fn init(&self, _ctx: &InitContext, config: Self::Config) -> crate::Result<Self::State> {
        self.validate.lock().expect("lock").push("init");
        Ok(config.value)
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        _inputs: &Self::Inputs,
    ) -> crate::Result<(Self::State, Self::Outputs)> {
        self.validate.lock().expect("lock").push("step");
        std::thread::sleep(self.step_delay);
        if self.fail_step {
            Err(anyhow::anyhow!("step failed"))
        } else {
            Ok((state + 1, TestOutputs { value: state + 1 }))
        }
    }
}

impl RegisteredRuntime for TestRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(10, 10, 100);

    fn __retain_artifact_metadata() {}
}

impl super::super::outputs::OutputBindings for TestRuntime {
    const FIELDS: &'static [super::super::outputs::OutputField] = &[];
}

#[test]
fn old_or_inexact_execution_semantics_are_refused_before_ready() {
    let manifest = RuntimeLaunchManifest {
        root: PathBuf::from("."),
        robot_id: "test".into(),
        instance_id: "brain".into(),
        executable: PathBuf::from("brain"),
        executable_sha256: "00".repeat(32),
        config: Value::Null,
        connections: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        scenario_producers: BTreeMap::new(),
        observation_providers: BTreeMap::new(),
    };
    let mut request = execution_wire::AdmitExecutionRequest {
        execution_id: "execution".into(),
        timeline_id: "timeline".into(),
        artifact_digest: vec![0; 32],
        required_contracts: vec![execution_wire::ContractRequirement {
            protocol: "phoxal.execution.v1".into(),
            capabilities: execution_protocol::REQUIRED_CAPABILITIES
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        }],
        mode: execution_wire::ExecutionMode::Controlled as i32,
        quantum_ns: 1_000_000,
    };
    let valid = |request: &execution_wire::AdmitExecutionRequest| {
        validate_execution_admission::<TestRuntime>(
            &manifest,
            request,
            execution_wire::ExecutionMode::Controlled,
            "execution",
        )
        .is_ok()
    };
    assert!(valid(&request));
    request.required_contracts[0]
        .capabilities
        .push("invocation".into());
    assert!(!valid(&request));
    request.required_contracts[0].capabilities =
        vec!["invocation".into(), "reset".into(), "delivery-ack".into()];
    assert!(!valid(&request));
}

struct TestInputsSource;

impl InputSource<TestRuntime> for TestInputsSource {
    fn freeze(&mut self, _candidate: &HardwareInvocation) -> crate::Result<TestInputs> {
        Ok(TestInputs)
    }
}

struct TestSink {
    reserve: bool,
    published: Vec<u64>,
    stopped: bool,
}

impl OutputAdmission<TestOutputs> for TestSink {
    type Reservation = u64;

    fn reserve(&mut self, outputs: &TestOutputs) -> crate::Result<Self::Reservation> {
        if self.reserve {
            Ok(outputs.value)
        } else {
            Err(anyhow::anyhow!("output capacity refused"))
        }
    }
}

impl OutputSink<TestRuntime> for TestSink {
    fn publish(
        &mut self,
        accepted: AcceptedInvocation<TestOutputs, Self::Reservation>,
    ) -> crate::Result<()> {
        self.published.push(accepted.outputs().value);
        Ok(())
    }

    fn stop(&mut self) -> crate::Result<()> {
        self.stopped = true;
        Ok(())
    }
}

#[test]
fn launch_manifest_reads_component_driver_configuration() {
    let temporary = std::env::temp_dir().join(format!(
        "phoxal-driver-manifest-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&temporary).expect("temporary driver bundle");
    let executable = temporary.join("bin/sensor");
    std::fs::create_dir_all(executable.parent().expect("driver parent")).expect("bin");
    let bytes = b"driver-fixture";
    std::fs::write(&executable, bytes).expect("driver executable");
    let manifest = serde_json::json!({
        "schema": "phoxal/bundle/v0",
        "robot_id": "driver-fixture",
        "document": {
            "robot": {
                "id": "driver-fixture",
                "components": {
                    "sensor": {
                        "component": "hardware-driver-fixture",
                        "mount_site": "sensor_mount",
                        "driver": {"config": {"value": 42}}
                    }
                }
            },
            "services": {},
            "connections": {}
        },
        "executables": [{
            "instance": "sensor",
            "path": "bin/sensor",
            "bytes": bytes.len(),
            "sha256": format!("{:x}", Sha256::digest(bytes))
        }]
    });
    std::fs::write(
        temporary.join("manifest.json"),
        serde_json::to_vec(&manifest).expect("manifest serializes"),
    )
    .expect("manifest writes");

    let launch =
        RuntimeLaunchManifest::open(&temporary, "sensor").expect("component driver bundle opens");
    let config: TestConfig = launch
        .decode_config::<TestRuntime>()
        .expect("component driver config decodes");
    assert_eq!(config.value, 42);
    std::fs::remove_dir_all(temporary).expect("temporary driver bundle removes");
}

#[test]
fn process_termination_boundary_cannot_return_a_terminal_operation_error() {
    let terminated = Arc::new(AtomicBool::new(false));
    let termination_flag = Arc::clone(&terminated);
    let error = anyhow::anyhow!(RunnerError::ProcessTerminationRequired {
        field: "operation",
        detail: "worker remained live".to_owned(),
    });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = enforce_process_boundary_with(Err(error), || {
            termination_flag.store(true, Ordering::SeqCst);
            panic!("test process terminator")
        });
    }));
    assert!(result.is_err());
    assert!(terminated.load(Ordering::SeqCst));

    let ordinary = anyhow::anyhow!("ordinary runtime failure");
    assert!(
        enforce_process_boundary_with(Err(ordinary), || {
            panic!("ordinary failures must remain returnable")
        })
        .is_err()
    );
}

#[test]
fn launch_parser_requires_explicit_bundle_instance_and_endpoint() {
    let parsed = RuntimeLaunch::try_parse_from([
        "runtime",
        "--bundle-root",
        "/tmp/bundle",
        "--instance-id",
        "motion",
        "--execution-id",
        "10000000000000000000000000000001",
        "--connect",
        "tcp/127.0.0.1:7447",
    ])
    .expect("launch parses");
    assert_eq!(parsed.instance_id, "motion");
    assert_eq!(
        parsed.execution_id.to_string(),
        "10000000000000000000000000000001"
    );
    assert!(RuntimeLaunch::try_parse_from(["runtime", "--bundle-root", "/tmp"]).is_err());
}

#[test]
fn runner_reserves_outputs_before_publishing_and_advancing() {
    let runtime = TestRuntime {
        validate: Arc::new(Mutex::new(Vec::new())),
        fail_step: false,
        step_delay: Duration::ZERO,
    };
    let mut runner = RuntimeRunner::new(
        runtime,
        ExecutionTime::default(),
        TestConfig { value: 1 },
        TestInputsSource,
        TestSink {
            reserve: true,
            published: Vec::new(),
            stopped: false,
        },
    )
    .expect("runner initializes");
    assert!(matches!(
        runner.poll(ExecutionTime::default()),
        Ok(PollOutcome::Accepted {
            invocation_index: 0
        })
    ));
    assert_eq!(runner.status(), RuntimeStatus::Ready);
}

#[test]
fn validation_runs_before_init_for_a_non_clone_configuration() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let runtime = TestRuntime {
        validate: Arc::clone(&order),
        fail_step: false,
        step_delay: Duration::ZERO,
    };
    RuntimeRunner::new(
        runtime,
        ExecutionTime::default(),
        TestConfig { value: 1 },
        TestInputsSource,
        TestSink {
            reserve: true,
            published: Vec::new(),
            stopped: false,
        },
    )
    .expect("valid non-clone config starts");
    assert_eq!(&*order.lock().expect("lock"), &["init"]);
}

#[test]
fn validation_failure_never_calls_init_for_an_owned_non_clone_config() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let runtime = TestRuntime {
        validate: Arc::clone(&order),
        fail_step: false,
        step_delay: Duration::ZERO,
    };
    assert!(
        RuntimeRunner::new(
            runtime,
            ExecutionTime::default(),
            TestConfig { value: 0 },
            TestInputsSource,
            TestSink {
                reserve: true,
                published: Vec::new(),
                stopped: false,
            },
        )
        .is_err()
    );
    assert!(order.lock().expect("lock").is_empty());
}

#[test]
fn output_refusal_faults_and_stops_the_runner_before_schedule_commit() {
    let runtime = TestRuntime {
        validate: Arc::new(Mutex::new(Vec::new())),
        fail_step: false,
        step_delay: Duration::ZERO,
    };
    let mut runner = RuntimeRunner::new(
        runtime,
        ExecutionTime::default(),
        TestConfig { value: 1 },
        TestInputsSource,
        TestSink {
            reserve: false,
            published: Vec::new(),
            stopped: false,
        },
    )
    .expect("runner initializes");
    assert!(runner.poll(ExecutionTime::default()).is_err());
    assert_eq!(runner.status(), RuntimeStatus::Failed);
    assert_eq!(runner.next_release(), ExecutionTime::default());
}

#[test]
fn process_step_failure_is_terminal_and_does_not_publish() {
    let runtime = TestRuntime {
        validate: Arc::new(Mutex::new(Vec::new())),
        fail_step: true,
        step_delay: Duration::ZERO,
    };
    let mut runner = RuntimeRunner::new(
        runtime,
        ExecutionTime::default(),
        TestConfig { value: 1 },
        TestInputsSource,
        TestSink {
            reserve: true,
            published: Vec::new(),
            stopped: false,
        },
    )
    .expect("runner initializes");
    assert!(runner.poll(ExecutionTime::default()).is_err());
    assert_eq!(runner.status(), RuntimeStatus::Failed);
}

#[test]
fn invocation_deadline_faults_the_owner_without_publishing() {
    let runtime = TestRuntime {
        validate: Arc::new(Mutex::new(Vec::new())),
        fail_step: false,
        step_delay: Duration::from_millis(20),
    };
    let mut runner = RuntimeRunner::new(
        runtime,
        ExecutionTime::default(),
        TestConfig { value: 1 },
        TestInputsSource,
        TestSink {
            reserve: true,
            published: Vec::new(),
            stopped: false,
        },
    )
    .expect("runner initializes");
    assert!(runner.poll(ExecutionTime::default()).is_err());
    assert_eq!(runner.status(), RuntimeStatus::Failed);
}

#[derive(Clone, PartialEq, Message)]
struct TransportRequest {
    #[prost(uint32, tag = "1")]
    value: u32,
}

impl prost::Name for TransportRequest {
    const NAME: &'static str = "TransportRequest";
    const PACKAGE: &'static str = "phoxal.runtime.test";

    fn full_name() -> String {
        "phoxal.runtime.test.TransportRequest".to_owned()
    }

    fn type_url() -> String {
        "/phoxal.runtime.test.TransportRequest".to_owned()
    }
}

#[derive(Clone, PartialEq, Message)]
struct TransportResponse {
    #[prost(uint32, tag = "1")]
    value: u32,
}

impl prost::Name for TransportResponse {
    const NAME: &'static str = "TransportResponse";
    const PACKAGE: &'static str = "phoxal.runtime.test";

    fn full_name() -> String {
        "phoxal.runtime.test.TransportResponse".to_owned()
    }

    fn type_url() -> String {
        "/phoxal.runtime.test.TransportResponse".to_owned()
    }
}

const TRANSPORT_PORT: crate::port::PortSignature = crate::port::PortSignature::with_descriptor(
    "transport-commands",
    "phoxal.runtime.test",
    "Transport",
    crate::port::PortKind::Commands,
    "phoxal.runtime.test.TransportRequest",
    "phoxal.runtime.test.TransportResponse",
    &[],
);

struct TransportInputs {
    commands: crate::runtime::Commands<TransportRequest, TransportResponse>,
}

impl crate::runtime::input::InputSet for TransportInputs {
    const FIELDS: &'static [crate::runtime::input::InputField] = &[];
    const TRANSPORT_FIELDS: &'static [crate::runtime::transport::InputTransportField] =
        &[crate::runtime::transport::InputTransportField {
            name: "commands",
            kind: crate::runtime::input::InputKind::Commands,
            signature: Some(TRANSPORT_PORT),
            max_age_ms: None,
            max_items: Some(4),
            max_bytes: Some(1024),
        }];

    fn decode_transport_field(
        &mut self,
        field: &str,
        mut samples: Vec<crate::runtime::transport::WireSample>,
    ) -> crate::Result<()> {
        if field != "commands" {
            return Err(anyhow::anyhow!("unexpected transport input field {field}"));
        }
        crate::runtime::transport::sort_command_samples(&mut samples)?;
        let mut items = Vec::with_capacity(samples.len());
        let mut bytes = 0_u64;
        for sample in samples {
            bytes = bytes
                .checked_add(sample.payload().len() as u64)
                .ok_or_else(|| {
                    anyhow::anyhow!(crate::runtime::transport::TransportError::BatchTooLarge {
                        port: TRANSPORT_PORT.name.to_owned(),
                        what: "encoded bytes",
                        actual: u64::MAX,
                        maximum: 1024,
                    })
                })?;
            if bytes > 1024 {
                return Err(anyhow::anyhow!(
                    crate::runtime::transport::TransportError::BatchTooLarge {
                        port: TRANSPORT_PORT.name.to_owned(),
                        what: "encoded bytes",
                        actual: bytes,
                        maximum: 1024,
                    }
                ));
            }
            let request: TransportRequest =
                crate::runtime::transport::decode_request(TRANSPORT_PORT, &sample, 1024)?;
            let order = crate::runtime::transport::command_order(sample.metadata())?;
            items.push(crate::runtime::Command::with_order(order, request));
        }
        self.commands = crate::runtime::Commands::bounded(
            items,
            bytes,
            crate::runtime::Capacity::new(4, 1024)?,
        )?;
        Ok(())
    }
}

impl InputSnapshot for TransportInputs {
    type Transport = ();

    fn empty() -> Self {
        Self {
            commands: crate::runtime::Commands::default(),
        }
    }
}

impl crate::runtime::input::TransportInputSet for TransportInputs {
    const TRANSPORT_FIELDS: &'static [crate::runtime::transport::InputTransportField] =
        <Self as crate::runtime::input::InputSet>::TRANSPORT_FIELDS;

    fn decode_transport_field(
        &mut self,
        field: &str,
        _binding: Option<&crate::runtime::transport::PortBinding>,
        samples: Vec<crate::runtime::transport::WireSample>,
    ) -> crate::Result<()> {
        <Self as crate::runtime::input::InputSet>::decode_transport_field(self, field, samples)
    }
}

impl crate::runtime::input::TransportInputSink for TransportInputs {
    fn set_latest(
        &mut self,
        field: &str,
        _value: crate::runtime::input::TransportValue,
        _stamp: ObservationStamp,
    ) -> crate::Result<()> {
        Err(anyhow::anyhow!(format!("unexpected latest field {field}")))
    }

    fn set_samples(
        &mut self,
        field: &str,
        _values: Vec<crate::runtime::input::TransportSample>,
        _gap: bool,
    ) -> crate::Result<()> {
        Err(anyhow::anyhow!(format!("unexpected samples field {field}")))
    }

    fn set_events(
        &mut self,
        field: &str,
        _values: Vec<crate::runtime::input::TransportValue>,
        _gap: bool,
    ) -> crate::Result<()> {
        Err(anyhow::anyhow!(format!("unexpected events field {field}")))
    }

    fn set_setpoint(
        &mut self,
        field: &str,
        _value: Option<(
            crate::runtime::input::TransportValue,
            ExecutionTime,
            ExecutionTime,
        )>,
    ) -> crate::Result<()> {
        Err(anyhow::anyhow!(format!(
            "unexpected setpoint field {field}"
        )))
    }

    fn set_stream(
        &mut self,
        field: &str,
        _values: Vec<crate::runtime::input::TransportStreamItem>,
    ) -> crate::Result<()> {
        Err(anyhow::anyhow!(format!("unexpected stream field {field}")))
    }

    fn set_commands(
        &mut self,
        field: &str,
        _values: Vec<crate::runtime::input::TransportCommand>,
        _encoded_bytes: u64,
    ) -> crate::Result<()> {
        Err(anyhow::anyhow!(format!(
            "unexpected commands sink field {field}"
        )))
    }

    fn set_read(
        &mut self,
        field: &str,
        _key: crate::runtime::input::TransportValue,
        _result: Result<crate::runtime::input::TransportValue, ReadError>,
    ) -> crate::Result<()> {
        Err(anyhow::anyhow!(format!("unexpected read field {field}")))
    }

    fn set_request(
        &mut self,
        field: &str,
        _key: crate::runtime::input::TransportValue,
        _result: Result<crate::runtime::input::TransportValue, RequestError>,
    ) -> crate::Result<()> {
        Err(anyhow::anyhow!(format!("unexpected request field {field}")))
    }

    fn set_operation(
        &mut self,
        field: &str,
        _key: crate::runtime::input::TransportValue,
        _result: Result<crate::runtime::input::TransportValue, OperationInputError>,
    ) -> crate::Result<()> {
        Err(anyhow::anyhow!(format!(
            "unexpected operation field {field}"
        )))
    }
}

#[derive(Default)]
struct TransportOutputs {
    replies: Vec<crate::runtime::Reply<TransportResponse>>,
}

impl crate::runtime::outputs::OutputSet for TransportOutputs {
    const FIELDS: &'static [crate::runtime::outputs::OutputField] = &[];

    fn encode_transport(
        &self,
        context: StepContext,
        _resolve_input_port: &dyn Fn(&str) -> Option<crate::port::PortSignature>,
        source: &str,
    ) -> crate::Result<Vec<crate::runtime::transport::PreparedOutput>> {
        let mut records = Vec::with_capacity(self.replies.len());
        let mut bytes = 0_usize;
        for reply in &self.replies {
            let record = crate::runtime::transport::PreparedOutput::reply(
                TRANSPORT_PORT,
                reply.response(),
                1024,
                crate::runtime::transport::reply_metadata_for_order(source, context, reply.order()),
            )?;
            bytes = crate::runtime::transport::checked_add_batch_bytes(
                TRANSPORT_PORT,
                bytes,
                record.payload_len(),
                4096,
            )?;
            records.push(record);
        }
        crate::runtime::transport::check_batch(TRANSPORT_PORT, records.len(), bytes, 4, 4096)?;
        Ok(records)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct TransportRuntime;

impl Runtime for TransportRuntime {
    type Config = ();
    type State = ();
    type Inputs = TransportInputs;
    type Outputs = TransportOutputs;

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
        Ok(())
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        inputs: &Self::Inputs,
    ) -> crate::Result<(Self::State, Self::Outputs)> {
        let mut outputs = TransportOutputs::default();
        for command in inputs.commands.items() {
            outputs.replies.push(command.reply(TransportResponse {
                value: command.request().value.saturating_add(1),
            }));
        }
        Ok((state, outputs))
    }
}

impl RegisteredRuntime for TransportRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(10, 100, 100);

    fn __retain_artifact_metadata() {}
}

impl crate::runtime::outputs::OutputBindings for TransportRuntime {
    const FIELDS: &'static [crate::runtime::outputs::OutputField] = &[];
}

static CADENCE_FIELDS: &[crate::runtime::outputs::OutputField] =
    &[crate::runtime::outputs::OutputField {
        name: "status",
        kind: crate::runtime::outputs::OutputKind::State,
        port: Some(TRANSPORT_PORT.name),
        port_signature: Some(TRANSPORT_PORT),
        input: None,
        project: None,
        max_items: None,
        max_bytes: Some(1024),
        max_request_bytes: None,
        every_steps: Some(5),
        on_change: false,
        bootstrap: true,
        valid_for_ms: None,
        timeout_ms: None,
        cancel_grace_ms: None,
    }];

struct CadenceRuntime;

impl Runtime for CadenceRuntime {
    type Config = TestConfig;
    type State = u64;
    type Inputs = TestInputs;
    type Outputs = TestOutputs;

    fn init(&self, _ctx: &InitContext, config: Self::Config) -> crate::Result<Self::State> {
        Ok(config.value)
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        _inputs: &Self::Inputs,
    ) -> crate::Result<(Self::State, Self::Outputs)> {
        Ok((state + 1, TestOutputs { value: state + 1 }))
    }
}

impl crate::runtime::outputs::OutputBindings for CadenceRuntime {
    const FIELDS: &'static [crate::runtime::outputs::OutputField] = CADENCE_FIELDS;
}

#[test]
fn bootstrap_and_every_steps_use_one_based_acceptance_cadence() {
    let mut adapter = ExecutionOutputAdapter::<CadenceRuntime>::unbound();
    let output = || {
        crate::runtime::transport::PreparedOutput::response(
            TRANSPORT_PORT,
            &TransportResponse { value: 1 },
            1024,
            crate::runtime::transport::RuntimeWireMetadata::data(
                "cadence",
                ExecutionTime::default(),
                1,
            ),
        )
        .expect("cadence output encodes")
        .for_field("status")
    };

    adapter.projections.push(output());
    adapter
        .filter_state_projections(
            StepContext::first(ExecutionTime::default(), ExecutionDuration::from_millis(10)),
            true,
        )
        .expect("bootstrap filters");
    assert_eq!(adapter.projections.len(), 1, "bootstrap is published first");
    adapter.projections.clear();

    for index in 0..10 {
        adapter.projections.push(output());
        adapter
            .filter_state_projections(
                StepContext::new(
                    ExecutionTime::from_nanos((index + 1) * 10_000_000),
                    ExecutionDuration::from_millis(10),
                    ExecutionDuration::from_millis(10),
                    0,
                    index,
                ),
                false,
            )
            .expect("cadence filters");
        let due = matches!(index, 4 | 9);
        assert_eq!(
            adapter.projections.len(),
            usize::from(due),
            "accepted invocation {index} cadence"
        );
        adapter.projections.clear();
    }
}

#[test]
fn future_commands_validate_before_retention_and_reject_replays() {
    fn sample(metadata: crate::runtime::transport::RuntimeWireMetadata) -> WireSample {
        let mut payload = Vec::new();
        TransportRequest { value: 7 }
            .encode(&mut payload)
            .expect("request encodes");
        WireSample::from_parts(
            payload,
            metadata,
            "runtime/target/ports/transport-commands/request",
        )
    }

    fn batch(sample: WireSample) -> CollectedInput {
        CollectedInput {
            field: "commands",
            binding: crate::runtime::transport::PortBinding::from_signature(TRANSPORT_PORT),
            direction: InputDirection::Request,
            max_items: 4,
            max_bytes: 1024,
            samples: vec![sample],
        }
    }

    let mut adapter = ExecutionInputAdapter::<TransportRuntime>::unbound();
    let mut malformed = crate::runtime::transport::RuntimeWireMetadata::external_command(
        ExecutionTime::default(),
        1,
        100,
        1,
    );
    malformed.eligible_boundary = None;
    let error = adapter
        .validate_future_command_admission(&batch(sample(malformed)))
        .expect_err("future records validate before retention");
    assert!(error.to_string().contains("eligible_boundary"));
    assert!(adapter.future_commands.is_empty());

    let valid = sample(
        crate::runtime::transport::RuntimeWireMetadata::external_command(
            ExecutionTime::default(),
            2,
            100,
            2,
        ),
    );
    adapter
        .future_commands
        .insert("commands", vec![valid.clone()]);
    let error = adapter
        .validate_future_command_admission(&batch(valid))
        .expect_err("replayed retained future records are rejected");
    assert!(error.to_string().contains("duplicate command id"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_prost_runtime_transport_round_trip_preserves_command_order() {
    use zenoh::Wait;
    use zenoh::bytes::Encoding;
    use zenoh::key_expr::OwnedKeyExpr;

    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("typed-runtime").expect("participant id"),
            Vec::new(),
        ),
    )
    .await
    .expect("test bus opens");
    let mut input = ExecutionInputAdapter::<TransportRuntime>::unbound();
    input
        .bind_direct(bus.clone(), "transport")
        .await
        .expect("input binds");
    let mut output = ExecutionOutputAdapter::<TransportRuntime>::unbound();
    output.bind_direct(bus.clone(), "transport");
    output.reply_callers = BTreeMap::from([
        (
            (TRANSPORT_PORT.name.to_owned(), 2),
            "caller-b.commands".to_owned(),
        ),
        (
            (TRANSPORT_PORT.name.to_owned(), 3),
            "caller-a.commands".to_owned(),
        ),
    ]);

    let session = bus.session().expect("session is open");
    let reply_key = bus.full_key(&crate::runtime::transport::port_key(
        "transport",
        TRANSPORT_PORT.name,
        "reply",
    ));
    let replies = session
        .declare_subscriber(OwnedKeyExpr::new(reply_key).expect("reply key"))
        .with(zenoh::handlers::FifoChannel::new(4))
        .await
        .expect("reply subscriber");

    let input_key = bus.full_key(&crate::runtime::transport::port_key(
        "transport",
        TRANSPORT_PORT.name,
        "request",
    ));
    let publish_request = |request: TransportRequest,
                           source: &str,
                           command_id: u64,
                           eligible_boundary: u64,
                           caller_rank: u64| {
        let mut payload = Vec::new();
        request.encode(&mut payload).expect("request encodes");
        let metadata = crate::runtime::transport::RuntimeWireMetadata::command(
            source,
            ExecutionTime::from_nanos(10),
            command_id,
            eligible_boundary,
            caller_rank,
        )
        .with_caller(format!("{source}.commands"))
        .encode_bounded()
        .expect("metadata encodes");
        session
            .put(input_key.clone(), payload)
            .encoding(Encoding::from(
                crate::runtime::transport::PROTOBUF_ENCODING.to_owned(),
            ))
            .attachment(metadata)
            .wait()
            .expect("request publishes");
    };
    let publish_external_request = |request: TransportRequest,
                                    command_id: u64,
                                    eligible_boundary: u64,
                                    ingress_sequence: u64| {
        let mut payload = Vec::new();
        request.encode(&mut payload).expect("request encodes");
        let metadata = crate::runtime::transport::RuntimeWireMetadata::external_command(
            ExecutionTime::from_nanos(10),
            command_id,
            eligible_boundary,
            ingress_sequence,
        )
        .encode_bounded()
        .expect("metadata encodes");
        session
            .put(input_key.clone(), payload)
            .encoding(Encoding::from(
                crate::runtime::transport::PROTOBUF_ENCODING.to_owned(),
            ))
            .attachment(metadata)
            .wait()
            .expect("external request publishes");
    };
    // Publish the higher-ranked caller first.  The frozen input cut must
    // still use the authoritative boundary/rank merge key, not Zenoh
    // arrival order.
    publish_request(TransportRequest { value: 50 }, "caller-b", 100, 0, 2);
    publish_request(TransportRequest { value: 41 }, "caller-a", 99, 0, 3);
    publish_external_request(TransportRequest { value: 60 }, 101, 0, 3);

    let mut runner = RuntimeRunner::new(
        TransportRuntime,
        ExecutionTime::default(),
        (),
        input,
        output,
    )
    .expect("runtime initializes");
    let first_poll = runner.poll(ExecutionTime::default());
    assert!(matches!(
        first_poll,
        Ok(PollOutcome::Accepted {
            invocation_index: 0
        })
    ));

    let mut received = Vec::new();
    for expected in [(100, 2, 51), (99, 3, 42), (101, 0, 61)] {
        let sample = tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
            .await
            .expect("reply arrives")
            .expect("reply receive succeeds");
        let wire = crate::runtime::transport::WireSample::from_zenoh(sample)
            .expect("reply has typed transport metadata");
        let response = TransportResponse::decode(wire.payload()).expect("response decodes");
        assert_eq!(response.value, expected.2);
        assert_eq!(wire.metadata().command_id, Some(expected.0));
        assert_eq!(wire.metadata().eligible_boundary, Some(0));
        if expected.0 == 101 {
            assert_eq!(wire.metadata().caller, Some("supervisor.public".to_owned()));
            assert_eq!(wire.metadata().ingress_sequence, Some(3));
            assert_eq!(wire.metadata().caller_rank, None);
        } else {
            assert_eq!(wire.metadata().caller_rank, Some(expected.1));
        }
        received.push(wire);
    }
    assert_eq!(received.len(), 3);

    // A request for a later eligible boundary remains retained without
    // entering the earlier frozen cuts, then becomes visible exactly at
    // its boundary.
    publish_external_request(TransportRequest { value: 70 }, 102, 17, 4);
    for index in 1..=17 {
        let now = ExecutionTime::from_nanos(index * 10_000_000);
        assert!(matches!(
            runner.poll(now),
            Ok(PollOutcome::Accepted { invocation_index }) if invocation_index == index
        ));
        if index < 17 {
            assert!(
                replies
                    .try_recv()
                    .expect("reply receive succeeds")
                    .is_none()
            );
        }
    }
    let future_reply = tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
        .await
        .expect("future reply arrives")
        .expect("future reply receive succeeds");
    let future_wire = crate::runtime::transport::WireSample::from_zenoh(future_reply)
        .expect("future reply has typed metadata");
    assert_eq!(future_wire.metadata().command_id, Some(102));
    assert_eq!(future_wire.metadata().eligible_boundary, Some(17));
    assert_eq!(future_wire.metadata().ingress_sequence, Some(4));

    // A late replay from the already admitted command must fail before a
    // second service step and must not produce a duplicate reply.
    publish_request(TransportRequest { value: 999 }, "caller-a", 99, 0, 3);
    assert!(runner.poll(ExecutionTime::from_nanos(10_000_000)).is_err());
    assert_eq!(runner.status(), RuntimeStatus::Failed);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), replies.recv_async())
            .await
            .is_err()
    );

    owner.close().await;
}

#[crate::runtime::inputs]
struct OperationInputs {
    operation: crate::runtime::Operation<u64, u32>,
}

struct OperationRuntime {
    completion: Arc<Mutex<Option<u32>>>,
}

impl Runtime for OperationRuntime {
    type Config = ();
    type State = ();
    type Inputs = OperationInputs;
    type Outputs = ();

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
        Ok(())
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        inputs: &Self::Inputs,
    ) -> crate::Result<(Self::State, Self::Outputs)> {
        if let Some(completion) = inputs.operation.new_completion() {
            let value = completion
                .result()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            *self.completion.lock().expect("operation completion lock") = Some(*value);
        }
        Ok((state, ()))
    }
}

impl RegisteredRuntime for OperationRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(1, 100, 100);

    fn __retain_artifact_metadata() {}
}

#[crate::runtime::outputs]
impl OperationRuntime {
    #[crate::runtime::outputs::activate(operation)]
    fn activate(&self, _state: &()) -> Option<crate::runtime::Activation<u64, u32>> {
        Some(crate::runtime::Activation::new(7, 41))
    }

    #[crate::runtime::outputs::operation(operation, timeout_ms = 100, cancel_grace_ms = 20)]
    fn run(input: u32) -> crate::Result<u32> {
        Ok(input + 1)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_operation_activation_dispatches_and_returns_typed_completion()
-> crate::Result<()> {
    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("typed-operation").expect("participant id"),
            Vec::new(),
        ),
    )
    .await
    .expect("test bus opens");
    let correlations = Arc::new(Mutex::new(BTreeMap::new()));
    let expired_correlations = Arc::new(Mutex::new(BTreeSet::new()));
    let operation_completions = Arc::new(Mutex::new(Vec::new()));
    let exchange_completions = Arc::new(Mutex::new(Vec::new()));
    let mut input = ExecutionInputAdapter::<OperationRuntime>::unbound().with_shared_state(
        Arc::clone(&correlations),
        Arc::clone(&expired_correlations),
        Arc::clone(&operation_completions),
        Arc::clone(&exchange_completions),
    );
    input
        .bind_direct(bus.clone(), "operation")
        .await
        .expect("input binds");
    let mut output = ExecutionOutputAdapter::<OperationRuntime>::unbound().with_shared_state(
        correlations,
        expired_correlations,
        operation_completions,
        exchange_completions,
    );
    output.bind_direct(bus.clone(), "operation");
    let completion = Arc::new(Mutex::new(None));
    let mut runner = RuntimeRunner::new(
        OperationRuntime {
            completion: Arc::clone(&completion),
        },
        ExecutionTime::default(),
        (),
        input,
        output,
    )
    .expect("runtime initializes");

    assert!(matches!(
        runner.poll(ExecutionTime::default()),
        Ok(PollOutcome::Accepted {
            invocation_index: 0
        })
    ));
    for index in 1..=20 {
        tokio::time::sleep(Duration::from_millis(2)).await;
        let now = ExecutionTime::from_nanos(index * 2_000_000);
        let _ = runner.poll(now)?;
        if completion
            .lock()
            .expect("operation completion lock")
            .is_some()
        {
            break;
        }
    }
    assert_eq!(
        *completion.lock().expect("operation completion lock"),
        Some(42)
    );
    runner.stop().expect("runner stops");
    owner.close().await;
    Ok(())
}

#[derive(Clone, PartialEq, Message)]
struct ReadRequest {
    #[prost(uint32, tag = "1")]
    value: u32,
}

impl prost::Name for ReadRequest {
    const NAME: &'static str = "ReadRequest";
    const PACKAGE: &'static str = "phoxal.runtime.test";

    fn full_name() -> String {
        "phoxal.runtime.test.ReadRequest".to_owned()
    }

    fn type_url() -> String {
        "/phoxal.runtime.test.ReadRequest".to_owned()
    }
}

#[derive(Clone, PartialEq, Message)]
struct ReadResponse {
    #[prost(uint32, tag = "1")]
    value: u32,
}

impl prost::Name for ReadResponse {
    const NAME: &'static str = "ReadResponse";
    const PACKAGE: &'static str = "phoxal.runtime.test";

    fn full_name() -> String {
        "phoxal.runtime.test.ReadResponse".to_owned()
    }

    fn type_url() -> String {
        "/phoxal.runtime.test.ReadResponse".to_owned()
    }
}

const READ_PORT: crate::port::Read<ReadRequest, ReadResponse> = crate::port::Read::with_signature(
    "read",
    "phoxal.runtime.test.Reader",
    "Current",
    "phoxal.runtime.test.ReadRequest",
    "phoxal.runtime.test.ReadResponse",
    &[],
);

#[derive(Default)]
struct PublicReadRuntime {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    delay: Duration,
}

impl Runtime for PublicReadRuntime {
    type Config = ();
    type State = u32;
    type Inputs = ReadClientInputs;
    type Outputs = ();

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
        Ok(41)
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        _inputs: &Self::Inputs,
    ) -> crate::Result<(Self::State, Self::Outputs)> {
        Ok((state.saturating_add(1), ()))
    }
}

impl RegisteredRuntime for PublicReadRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(10, 1000, 100);

    fn __retain_artifact_metadata() {}
}

#[crate::runtime::outputs]
impl PublicReadRuntime {
    fn view(&self, state: &u32) -> u32 {
        *state
    }

    #[crate::runtime::outputs::read(
            port = READ_PORT,
            project = Self::view,
            max_request_bytes = 64,
            max_response_bytes = 4,
        )]
    fn inspect(&self, view: &u32, request: &ReadRequest) -> ReadResponse {
        self.calls.fetch_add(1, Ordering::Release);
        std::thread::sleep(self.delay);
        ReadResponse {
            value: view.saturating_add(request.value),
        }
    }
}

#[test]
fn offered_read_retains_both_request_and_response_bounds_in_its_artifact() {
    let field = <PublicReadRuntime as super::super::outputs::OutputBindings>::FIELDS
        .iter()
        .find(|field| field.name == "inspect")
        .unwrap();
    assert_eq!(field.max_request_bytes, Some(64));
    assert_eq!(field.max_bytes, Some(4));
}

#[crate::runtime::inputs]
struct ReadClientInputs {
    #[crate::runtime::input(max_response_bytes = 64)]
    read: crate::runtime::Read<u64, ReadRequest, ReadResponse>,
}

struct ReadClientRuntime {
    selected_key: Arc<Mutex<Option<u64>>>,
    response: Arc<Mutex<Option<u32>>>,
}

impl Runtime for ReadClientRuntime {
    type Config = ();
    type State = ();
    type Inputs = ReadClientInputs;
    type Outputs = ();

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
        Ok(())
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        inputs: &Self::Inputs,
    ) -> crate::Result<(Self::State, Self::Outputs)> {
        if let Some(completion) = inputs.read.new_completion() {
            let response = completion
                .result()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            *self.response.lock().expect("read response lock") = Some(response.value);
        }
        Ok((state, ()))
    }
}

impl RegisteredRuntime for ReadClientRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(1, 100, 100);

    fn __retain_artifact_metadata() {}
}

#[crate::runtime::outputs]
impl ReadClientRuntime {
    #[crate::runtime::outputs::activate(read, timeout_ms = 100)]
    fn request(&self, _state: &()) -> Option<crate::runtime::Activation<u64, ReadRequest>> {
        self.selected_key
            .lock()
            .unwrap()
            .map(|key| crate::runtime::Activation::new(key, ReadRequest { value: 41 }))
    }
}

async fn read_client_runner(
    bus: &crate::runtime::connection::Connection,
    manifest: &RuntimeLaunchManifest,
    selected_key: Arc<Mutex<Option<u64>>>,
    response: Arc<Mutex<Option<u32>>>,
) -> crate::Result<
    RuntimeRunner<
        ReadClientRuntime,
        ExecutionInputAdapter<ReadClientRuntime>,
        ExecutionOutputAdapter<ReadClientRuntime>,
    >,
> {
    // The input and output adapters must share one correlation state.
    let correlations = Arc::new(Mutex::new(BTreeMap::new()));
    let expired = Arc::new(Mutex::new(BTreeSet::new()));
    let operations = Arc::new(Mutex::new(Vec::new()));
    let exchanges = Arc::new(Mutex::new(Vec::new()));
    let mut input = ExecutionInputAdapter::<ReadClientRuntime>::unbound().with_shared_state(
        Arc::clone(&correlations),
        Arc::clone(&expired),
        Arc::clone(&operations),
        Arc::clone(&exchanges),
    );
    input.bind(bus.clone(), manifest).await?;
    let mut output = ExecutionOutputAdapter::<ReadClientRuntime>::unbound().with_shared_state(
        correlations,
        expired,
        operations,
        exchanges,
    );
    output
        .bind(bus.clone(), &manifest.instance_id, manifest)
        .await?;

    RuntimeRunner::new(
        ReadClientRuntime {
            selected_key,
            response,
        },
        ExecutionTime::default(),
        (),
        input,
        output,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_read_activation_uses_graph_target_and_correlated_reply() -> crate::Result<()> {
    use zenoh::Wait;
    use zenoh::bytes::Encoding;
    use zenoh::key_expr::OwnedKeyExpr;

    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("read-client").expect("participant id"),
            Vec::new(),
        ),
    )
    .await
    .expect("test bus opens");
    let signature = SourcePortSignature {
        name: READ_PORT.signature().name.to_owned(),
        service: READ_PORT.signature().service.to_owned(),
        method: READ_PORT.signature().method.to_owned(),
        kind: READ_PORT.signature().kind.as_str().to_owned(),
        request: READ_PORT.signature().request.to_owned(),
        response: READ_PORT.signature().response.to_owned(),
    };
    let mut connections = BTreeMap::new();
    connections.insert(
        "read-client.read".to_owned(),
        vec!["reader.read".to_owned()],
    );
    connections.insert("other.read".to_owned(), vec!["reader.read".to_owned()]);
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        "reader".to_owned(),
        SourceRuntimeRecord {
            period_ms: Some(1),
            timeout_ms: Some(100),
            init_timeout_ms: Some(100),
            inputs: Vec::new(),
            transient_outputs: Vec::new(),
            service_outputs: vec![SourceOutputRecord {
                name: "current".to_owned(),
                kind: "read".to_owned(),
                port: Some("read".to_owned()),
                signature: Some(signature),
                input: None,
                max_items: Some(1),
                max_bytes: Some(64),
                max_request_bytes: Some(64),
            }],
        },
    );
    let manifest = RuntimeLaunchManifest {
        root: PathBuf::from("."),
        robot_id: "typed-read-test".to_owned(),
        instance_id: "read-client".to_owned(),
        executable: PathBuf::from("typed-read-test"),
        executable_sha256: "00".repeat(32),
        config: Value::Object(serde_json::Map::new()),
        connections,
        artifacts,
        scenario_producers: BTreeMap::new(),
        observation_providers: BTreeMap::new(),
    };

    let session = bus.session().expect("session is open");
    let request_key = bus.full_key(&crate::runtime::transport::port_key(
        "reader",
        READ_PORT.name(),
        "read-request",
    ));
    let requests = session
        .declare_subscriber(OwnedKeyExpr::new(request_key).expect("request key"))
        .with(zenoh::handlers::FifoChannel::new(2))
        .await
        .expect("request subscriber");
    let response = Arc::new(Mutex::new(None));
    let selected_key = Arc::new(Mutex::new(Some(9)));
    let mut runner = read_client_runner(
        &bus,
        &manifest,
        Arc::clone(&selected_key),
        Arc::clone(&response),
    )
    .await?;
    assert!(matches!(
        runner.poll(ExecutionTime::default()),
        Ok(PollOutcome::Accepted {
            invocation_index: 0
        })
    ));
    let request = tokio::time::timeout(Duration::from_secs(2), requests.recv_async())
        .await
        .expect("read request arrives")
        .expect("read request receive succeeds");
    let wire = crate::runtime::transport::WireSample::from_zenoh(request)?;
    let request = ReadRequest::decode(wire.payload()).expect("request decodes");
    assert_eq!(request.value, 41);
    let metadata = wire.metadata();
    let mut other_manifest = manifest.clone();
    other_manifest.instance_id = "other".to_owned();
    let other_key = Arc::new(Mutex::new(Some(99)));
    let other_response = Arc::new(Mutex::new(None));
    let mut other = read_client_runner(
        &bus,
        &other_manifest,
        Arc::clone(&other_key),
        Arc::clone(&other_response),
    )
    .await?;
    other.poll(ExecutionTime::default())?;
    let other_request = tokio::time::timeout(Duration::from_secs(2), requests.recv_async())
        .await?
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let other_wire = WireSample::from_zenoh(other_request)?;
    assert_eq!(
        other_wire.metadata().command_id,
        metadata.command_id,
        "different callers may use the same local correlation id"
    );
    assert_ne!(other_wire.metadata().caller_rank, metadata.caller_rank);

    runner.poll(ExecutionTime::from_nanos(1_000_000))?;
    assert!(
        requests
            .try_recv()
            .expect("request queue readable")
            .is_none(),
        "same pending key must not resend"
    );
    let payload = crate::runtime::transport::encode_prost(&ReadResponse { value: 42 })?;
    let reply_metadata = crate::runtime::transport::reply_metadata(
        "reader",
        StepContext::first(
            ExecutionTime::from_nanos(2_000_000),
            ExecutionDuration::from_millis(1),
        ),
        metadata.command_id.expect("command id"),
        metadata.eligible_boundary.expect("boundary"),
        metadata.caller_rank.expect("caller rank"),
    )
    .with_caller(metadata.caller.clone().expect("request caller"))
    .encode_bounded()
    .expect("reply metadata");
    let response_key = bus.full_key(&crate::runtime::transport::port_key(
        "reader",
        READ_PORT.name(),
        "reply",
    ));
    session
        .put(response_key, payload)
        .encoding(Encoding::from(
            crate::runtime::transport::PROTOBUF_ENCODING.to_owned(),
        ))
        .attachment(reply_metadata)
        .wait()
        .expect("reply publishes");
    tokio::time::sleep(Duration::from_millis(10)).await;
    other.poll(ExecutionTime::from_nanos(1_000_000))?;
    assert_eq!(
        *other_response.lock().unwrap(),
        None,
        "a peer's reply cannot complete another caller's request"
    );
    *other_key.lock().unwrap() = None;
    other.poll(ExecutionTime::from_nanos(2_000_000))?;
    other.stop()?;
    // The reply is queued while this owner is not due. Admission must
    // stop the 100 ms transfer timeout before a later invocation consumes it.
    tokio::time::sleep(Duration::from_millis(120)).await;
    let _ = runner.poll(ExecutionTime::from_nanos(2_000_000))?;
    assert_eq!(*response.lock().expect("read response lock"), Some(42));
    runner.poll(ExecutionTime::from_nanos(3_000_000))?;
    assert!(
        requests
            .try_recv()
            .expect("request queue readable")
            .is_none(),
        "same completed key must not resend"
    );
    *selected_key.lock().unwrap() = None;
    runner.poll(ExecutionTime::from_nanos(4_000_000))?;
    *selected_key.lock().unwrap() = Some(9);
    runner.poll(ExecutionTime::from_nanos(5_000_000))?;
    let retry = tokio::time::timeout(Duration::from_secs(2), requests.recv_async())
        .await?
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let retry = WireSample::from_zenoh(retry)?;
    assert_ne!(
        retry.metadata().command_id,
        metadata.command_id,
        "reactivation after None starts a fresh attempt"
    );
    *selected_key.lock().unwrap() = Some(10);
    runner.poll(ExecutionTime::from_nanos(6_000_000))?;
    let replacement = tokio::time::timeout(Duration::from_secs(2), requests.recv_async())
        .await?
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let replacement = WireSample::from_zenoh(replacement)?;
    assert_ne!(
        replacement.metadata().command_id,
        retry.metadata().command_id
    );
    runner.poll(ExecutionTime::from_nanos(7_000_000))?;
    assert!(
        requests
            .try_recv()
            .expect("request queue readable")
            .is_none(),
        "replacement key also starts once"
    );
    runner.stop()?;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_read_uses_authenticated_external_ingress() -> crate::Result<()> {
    use zenoh::Wait;
    use zenoh::bytes::Encoding;
    use zenoh::key_expr::OwnedKeyExpr;

    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("public-reader").expect("participant id"),
            Vec::new(),
        ),
    )
    .await
    .expect("test bus opens");
    let manifest = RuntimeLaunchManifest {
        root: PathBuf::from("."),
        robot_id: "public-read-test".to_owned(),
        instance_id: "public-reader".to_owned(),
        executable: PathBuf::from("public-read-test"),
        executable_sha256: "00".repeat(32),
        config: Value::Object(serde_json::Map::new()),
        connections: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        scenario_producers: BTreeMap::new(),
        observation_providers: BTreeMap::new(),
    };
    let mut input = ExecutionInputAdapter::<PublicReadRuntime>::unbound();
    input.bind_direct(bus.clone(), "public-reader").await?;
    let mut output = ExecutionOutputAdapter::<PublicReadRuntime>::unbound();
    output.bind(bus.clone(), "public-reader", &manifest).await?;

    let mut runner = RuntimeRunner::new(
        PublicReadRuntime::default(),
        ExecutionTime::default(),
        (),
        input,
        output,
    )?;
    let session = bus.session().expect("session is open");
    let reply_key = bus.full_key(&crate::runtime::transport::port_key(
        "public-reader",
        READ_PORT.name(),
        "reply",
    ));
    let replies = session
        .declare_subscriber(OwnedKeyExpr::new(reply_key).expect("reply key"))
        .with(zenoh::handlers::FifoChannel::new(2))
        .await
        .expect("reply subscriber");
    let request_key = bus.full_key(&crate::runtime::transport::port_key(
        "public-reader",
        READ_PORT.name(),
        "request",
    ));
    let mut request_payload = Vec::new();
    ReadRequest { value: 1 }.encode(&mut request_payload)?;
    let request_metadata = crate::runtime::transport::RuntimeWireMetadata::external_request(
        ExecutionTime::default(),
        7,
        0,
        12,
    )
    .encode_bounded()
    .expect("external read metadata");
    session
        .put(request_key, request_payload)
        .encoding(Encoding::from(
            crate::runtime::transport::PROTOBUF_ENCODING.to_owned(),
        ))
        .attachment(request_metadata)
        .wait()
        .expect("external read publishes");

    let reply = tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
        .await
        .expect("public read reply arrives")
        .expect("public read reply receive succeeds");
    let wire = crate::runtime::transport::WireSample::from_zenoh(reply)?;
    let response = ReadResponse::decode(wire.payload()).expect("response decodes");
    assert_eq!(response.value, 42);
    assert_eq!(wire.metadata().source.as_deref(), Some("public-reader"));
    assert_eq!(wire.metadata().caller.as_deref(), Some("supervisor.public"));
    assert_eq!(wire.metadata().caller_rank, None);
    assert_eq!(wire.metadata().ingress_sequence, Some(12));
    runner.stop()?;
    owner.close().await;
    Ok(())
}

#[derive(Clone, PartialEq, Message)]
struct TypedState {
    #[prost(int32, tag = "1")]
    value: i32,
}

impl prost::Name for TypedState {
    const NAME: &'static str = "TypedState";
    const PACKAGE: &'static str = "phoxal.runtime.test";

    fn full_name() -> String {
        "phoxal.runtime.test.TypedState".to_owned()
    }

    fn type_url() -> String {
        "/phoxal.runtime.test.TypedState".to_owned()
    }
}

#[crate::runtime::inputs]
struct SetpointTestInputs {
    target: crate::runtime::Setpoint<TypedState>,
}

#[test]
fn empty_protobuf_setpoint_is_a_value_and_withdrawal_is_explicit() {
    let signature = crate::port::PortSignature::with_descriptor(
        "target",
        "test.Service",
        "Target",
        crate::port::PortKind::Setpoint,
        "google.protobuf.Empty",
        "phoxal.runtime.test.TypedState",
        &[],
    );
    let binding = transport::PortBinding::from_signature(signature);
    let mut metadata = RuntimeWireMetadata::data("source", ExecutionTime::default(), 1);
    metadata.expires_at_nanos = Some(100);
    let mut inputs = SetpointTestInputs {
        target: crate::runtime::Setpoint::withdrawn(),
    };
    TransportInputSet::decode_transport_field(
        &mut inputs,
        "target",
        Some(&binding),
        vec![WireSample::from_parts(vec![], metadata.clone(), "test")],
    )
    .unwrap();
    assert_eq!(inputs.target.value().unwrap().value, 0);
    assert!(
        PreparedOutput::withdrawal(signature, metadata.clone())
            .actuation()
            .is_none()
    );
    metadata.control = transport::WireControl::Withdraw as u32;
    TransportInputSet::decode_transport_field(
        &mut inputs,
        "target",
        Some(&binding),
        vec![WireSample::from_parts(vec![], metadata.clone(), "test")],
    )
    .unwrap();
    assert!(inputs.target.value().is_none());
    metadata.control = transport::WireControl::Data as u32;
    metadata.expires_at_nanos = None;
    assert!(
        TransportInputSet::decode_transport_field(
            &mut inputs,
            "target",
            Some(&binding),
            vec![WireSample::from_parts(vec![], metadata, "test")]
        )
        .is_err()
    );
}

const SOURCE_STATE: crate::port::PortSignature = crate::port::PortSignature::with_descriptor(
    "state",
    "phoxal.runtime.test",
    "State",
    crate::port::PortKind::State,
    "google.protobuf.Empty",
    "phoxal.runtime.test.TypedState",
    &[],
);

#[crate::runtime::inputs]
struct TypedStateInputs {
    state: crate::runtime::Latest<TypedState>,
}

struct TypedStateRuntime {
    seen: Arc<Mutex<Option<i32>>>,
}

impl Runtime for TypedStateRuntime {
    type Config = ();
    type State = ();
    type Inputs = TypedStateInputs;
    type Outputs = ();

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
        Ok(())
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        inputs: &Self::Inputs,
    ) -> crate::Result<(Self::State, Self::Outputs)> {
        let value = inputs
            .state
            .value()
            .ok_or_else(|| anyhow::anyhow!("typed state was not admitted"))?;
        *self.seen.lock().expect("typed state observation lock") = Some(value.value);
        Ok((state, ()))
    }
}

impl RegisteredRuntime for TypedStateRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(10, 100, 100);

    fn __retain_artifact_metadata() {}
}

impl crate::runtime::outputs::OutputBindings for TypedStateRuntime {
    const FIELDS: &'static [crate::runtime::outputs::OutputField] = &[];
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_nonempty_state_transport_uses_manifest_connection() -> crate::Result<()> {
    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("typed-state").expect("participant id"),
            Vec::new(),
        ),
    )
    .await
    .expect("test bus opens");
    let signature = SourcePortSignature {
        name: SOURCE_STATE.name.to_owned(),
        service: SOURCE_STATE.service.to_owned(),
        method: SOURCE_STATE.method.to_owned(),
        kind: SOURCE_STATE.kind.as_str().to_owned(),
        request: SOURCE_STATE.request.to_owned(),
        response: SOURCE_STATE.response.to_owned(),
    };
    let mut connections = BTreeMap::new();
    connections.insert(
        "consumer.state".to_owned(),
        vec!["producer.state".to_owned()],
    );
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        "producer".to_owned(),
        SourceRuntimeRecord {
            period_ms: Some(10),
            timeout_ms: Some(100),
            init_timeout_ms: Some(100),
            inputs: Vec::new(),
            transient_outputs: Vec::new(),
            service_outputs: vec![SourceOutputRecord {
                name: "state".to_owned(),
                kind: "state".to_owned(),
                port: Some("state".to_owned()),
                signature: Some(signature),
                input: None,
                max_items: Some(1),
                max_bytes: Some(64),
                max_request_bytes: None,
            }],
        },
    );
    let manifest = RuntimeLaunchManifest {
        root: PathBuf::from("."),
        robot_id: "typed-test".to_owned(),
        instance_id: "consumer".to_owned(),
        executable: PathBuf::from("typed-test"),
        executable_sha256: "00".repeat(32),
        config: Value::Object(serde_json::Map::new()),
        connections,
        artifacts,
        scenario_producers: BTreeMap::new(),
        observation_providers: BTreeMap::new(),
    };
    let mut input = ExecutionInputAdapter::<TypedStateRuntime>::unbound();
    input
        .bind(bus.clone(), &manifest)
        .await
        .expect("input binds");
    let mut output = ExecutionOutputAdapter::<TypedStateRuntime>::unbound();
    output.bind_direct(bus.clone(), "consumer");

    let prepared = PreparedOutput::response(
        SOURCE_STATE,
        &TypedState { value: 42 },
        64,
        crate::runtime::transport::publication_metadata(
            "producer",
            StepContext::first(ExecutionTime::default(), ExecutionDuration::from_millis(10)),
            0,
        ),
    )?;
    crate::runtime::transport::publish_batch(&bus, "producer", &[prepared])?;
    let seen = Arc::new(Mutex::new(None));
    let mut runner = RuntimeRunner::new(
        TypedStateRuntime { seen: seen.clone() },
        ExecutionTime::default(),
        (),
        input,
        output,
    )
    .expect("runtime initializes");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(matches!(
        runner.poll(ExecutionTime::default()),
        Ok(PollOutcome::Accepted {
            invocation_index: 0
        })
    ));
    assert_eq!(
        *seen.lock().expect("typed state observation lock"),
        Some(42)
    );
    runner.stop().expect("runner stops");
    owner.close().await;
    Ok(())
}

#[test]
fn reply_receipts_preserve_the_source_port_and_select_only_the_admitted_caller() {
    let mut reply = PreparedOutput::reply(
        TRANSPORT_PORT,
        &TransportResponse { value: 42 },
        64,
        crate::runtime::transport::reply_metadata(
            "server",
            StepContext::first(ExecutionTime::default(), ExecutionDuration::from_millis(1)),
            1,
            1,
            3,
        ),
    )
    .unwrap();
    let callers = BTreeMap::from([(
        (TRANSPORT_PORT.name.to_owned(), 3),
        "alice.request".to_owned(),
    )]);
    reply.bind_reply_caller(&callers).unwrap();
    let receipt = reply.delivery_receipt(0).unwrap();
    assert_eq!(receipt.0, TRANSPORT_PORT.name);
    assert_eq!(receipt.1, "reply");
    assert_eq!(receipt.2.as_deref(), Some("alice.request"));
    let different = BTreeMap::from([(
        (TRANSPORT_PORT.name.to_owned(), 3),
        "bob.request".to_owned(),
    )]);
    assert!(reply.bind_reply_caller(&different).is_err());
    assert!(reply.bind_reply_caller(&BTreeMap::new()).is_err());
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_receiver_fields_acknowledge_independently_when_one_queue_is_full()
-> crate::Result<()> {
    use super::input::{DeliveryReceiver, delivery_receive_loop};
    let execution = crate::identity::ExecutionId::mint();
    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            execution,
            crate::identity::ParticipantId::new("fanout")?,
            Vec::new(),
        ),
    )
    .await?;
    let acknowledgements = declare_execution_subscriber(&bus, "producer", "delivery-ack").await?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut workers = Vec::new();
    let mut queues = Vec::new();
    for field in ["near", "far"] {
        let mut queue = DeliveryQueue::new(1, 64, crate::runtime::input::InputKind::Samples);
        queue.set_timeline("timeline");
        let queue = Arc::new(Mutex::new(queue));
        queues.push(Arc::clone(&queue));
        let subscriber = bus
            .session()?
            .declare_subscriber(bus.full_key(&transport::port_key(
                "producer",
                SOURCE_STATE.name,
                "publish",
            )))
            .with(zenoh::handlers::FifoChannel::new(2))
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        workers.push(tokio::spawn(delivery_receive_loop(DeliveryReceiver {
            subscriber,
            queue,
            bus: bus.clone(),
            expected_source: "producer".to_owned(),
            expected_callers: BTreeSet::new(),
            target: format!("consumer.{field}"),
            port: SOURCE_STATE.name.to_owned(),
            direction: "publish".to_owned(),
            ack_leg: "delivery-ack".to_owned(),
            cancel: cancel.clone(),
            reply_admission: None,
        })));
    }
    for boundary in [1, 2] {
        let mut output = PreparedOutput::response(
            SOURCE_STATE,
            &TypedState { value: 42 },
            64,
            RuntimeWireMetadata::data("producer", ExecutionTime::default(), boundary),
        )?;
        output.stamp_delivery_identity(&execution.to_string(), "timeline", boundary, 0)?;
        transport::publish_batch(&bus, "producer", &[output])?;
        let mut admitted = BTreeMap::new();
        for _ in 0..2 {
            let ack: execution_wire::DeliveryAck =
                tokio::time::timeout(Duration::from_secs(2), recv_execution(&acknowledgements))
                    .await??;
            assert_eq!(ack.boundary, boundary);
            assert!(admitted.insert(ack.target, ack.admitted).is_none());
        }
        assert!(admitted["consumer.near"]);
        assert_eq!(admitted["consumer.far"], boundary == 1);
        // Only near consumes its first cut. Far stays charged while paused.
        queues[0].lock().unwrap().drain(boundary + 1);
    }
    cancel.cancel();
    for worker in workers {
        worker.await??;
    }
    owner.close().await;
    Ok(())
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn controlled_read_pins_entry_state_and_waits_for_reply_receiver_admission()
-> crate::Result<()> {
    let execution = crate::identity::ExecutionId::mint();
    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            execution,
            crate::identity::ParticipantId::new("reader")?,
            Vec::new(),
        ),
    )
    .await?;
    let manifest = RuntimeLaunchManifest {
        root: PathBuf::from("."),
        robot_id: "read-proof".to_owned(),
        instance_id: "reader".to_owned(),
        executable: PathBuf::from("reader"),
        executable_sha256: "00".repeat(32),
        config: Value::Object(serde_json::Map::new()),
        connections: BTreeMap::from([("caller.read".to_owned(), vec!["reader.read".to_owned()])]),
        artifacts: BTreeMap::new(),
        scenario_producers: BTreeMap::new(),
        observation_providers: BTreeMap::new(),
    };
    let mut input = ExecutionInputAdapter::<PublicReadRuntime>::unbound();
    input.bind_direct(bus.clone(), "reader").await?;
    let mut output = ExecutionOutputAdapter::<PublicReadRuntime>::unbound();
    output.bind(bus.clone(), "reader", &manifest).await?;
    let mut runner = RuntimeRunner::new(
        PublicReadRuntime::default(),
        ExecutionTime::default(),
        (),
        input,
        output,
    )?;
    runner.set_controlled_timeline("timeline")?;
    runner.outputs.pin_read_views("timeline", 1)?;
    runner.invoke_controlled(1, ExecutionTime::from_nanos(10_000_000), "timeline")?;
    // A duplicate pin cannot replace the entry view with this boundary's new state.
    runner.outputs.pin_read_views("timeline", 1)?;
    let replies = bus
        .session()?
        .declare_subscriber(bus.full_key(&transport::port_key("reader", "read", "reply")))
        .with(zenoh::handlers::FifoChannel::new(2))
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let request_acks = declare_execution_subscriber(&bus, "caller", "delivery-ack").await?;
    for (boundary, expected_value, expected_capture_ns) in [(1, 42, 0), (2, 43, 10_000_000)] {
        if boundary == 2 {
            runner.outputs.pin_read_views("timeline", 2)?;
        }
        let mut metadata = transport::request_metadata(
            "caller",
            "caller.read",
            StepContext::first(ExecutionTime::default(), ExecutionDuration::from_millis(10)),
            boundary,
            boundary + 1,
            0,
        );
        metadata.request_timeout_ms = Some(500);
        let mut request = PreparedOutput::request(
            READ_PORT.signature(),
            &ReadRequest { value: 1 },
            64,
            metadata,
        )?
        .for_instance("reader");
        request.stamp_delivery_identity(&execution.to_string(), "timeline", boundary, 0)?;
        request.publish_async(&bus, "caller").await?;
        let reply = tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
            .await?
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let wire = WireSample::from_zenoh(reply)?;
        assert_eq!(ReadResponse::decode(wire.payload())?.value, expected_value);
        assert_eq!(
            wire.metadata().logical_time_nanos,
            Some(expected_capture_ns)
        );
        assert_eq!(wire.metadata().eligible_boundary, Some(boundary + 1));
        assert!(
            tokio::time::timeout(Duration::from_millis(25), request_acks.recv_async())
                .await
                .is_err(),
            "query completion alone cannot satisfy required receiver admission"
        );
        let ack = execution_wire::DeliveryAck {
            execution_id: execution.to_string(),
            timeline_id: "timeline".to_owned(),
            boundary,
            source: "reader".to_owned(),
            target: "caller.read".to_owned(),
            port: "read".to_owned(),
            direction: "reply".to_owned(),
            sequence: boundary + 1,
            item: 0,
            bytes: wire.payload().len() as u64,
            admitted: true,
            detail: None,
        };
        publish_execution(&bus, "reader", "read-reply-ack/read", &ack).await?;
        let admitted: execution_wire::DeliveryAck =
            tokio::time::timeout(Duration::from_secs(2), recv_execution(&request_acks)).await??;
        assert!(admitted.admitted);
        assert_eq!(admitted.source, "caller");
        assert_eq!(admitted.target, "reader.inspect");
        assert_eq!(admitted.boundary, boundary);
    }
    runner.stop()?;
    owner.close().await;
    Ok(())
}

async fn send_external_read(
    bus: &crate::runtime::connection::Connection,
    id: u64,
    value: u32,
) -> crate::Result<()> {
    let metadata = RuntimeWireMetadata::external_request(ExecutionTime::default(), id, 0, id);
    bus.session()?
        .put(
            bus.full_key(&transport::port_key("reader", "read", "request")),
            ReadRequest { value }.encode_to_vec(),
        )
        .encoding(zenoh::bytes::Encoding::from(
            transport::PROTOBUF_ENCODING.to_owned(),
        ))
        .attachment(metadata.encode_bounded()?)
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(())
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn immutable_reads_bound_busy_queries_and_retire_views_across_reset_and_stop()
-> crate::Result<()> {
    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("reader")?,
            Vec::new(),
        ),
    )
    .await?;
    let manifest = RuntimeLaunchManifest {
        root: PathBuf::from("."),
        robot_id: "read-proof".to_owned(),
        instance_id: "reader".to_owned(),
        executable: PathBuf::from("reader"),
        executable_sha256: "00".repeat(32),
        config: Value::Object(serde_json::Map::new()),
        connections: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        scenario_producers: BTreeMap::new(),
        observation_providers: BTreeMap::new(),
    };
    let mut input = ExecutionInputAdapter::<PublicReadRuntime>::unbound();
    input.bind_direct(bus.clone(), "reader").await?;
    let mut output = ExecutionOutputAdapter::<PublicReadRuntime>::unbound();
    output.bind(bus.clone(), "reader", &manifest).await?;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let service = PublicReadRuntime {
        calls: calls.clone(),
        delay: Duration::from_millis(100),
    };
    let mut runner = RuntimeRunner::new(service, ExecutionTime::default(), (), input, output)?;
    let replies = bus
        .session()?
        .declare_subscriber(bus.full_key(&transport::port_key("reader", "read", "reply")))
        .with(zenoh::handlers::FifoChannel::new(8))
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    // Ordinary producer acknowledgements must not accumulate in idle query workers.
    let flood = tokio::time::timeout(Duration::from_secs(2), async {
        for item in 0..128 {
            publish_execution(
                &bus,
                "reader",
                "delivery-ack",
                &execution_wire::DeliveryAck {
                    item,
                    ..Default::default()
                },
            )
            .await?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
    if flood.is_err() {
        runner.stop()?;
        owner.close().await;
        anyhow::bail!("idle Read subscriptions blocked ordinary acknowledgement traffic");
    }
    flood??;
    let wait_entered = |count| {
        let calls = calls.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(2), async {
                while calls.load(Ordering::Acquire) < count {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
        }
    };
    send_external_read(&bus, 1, 1).await?;
    wait_entered(1).await?;
    runner.poll(ExecutionTime::default())?;
    send_external_read(&bus, 2, 1).await?;
    let mut received = BTreeMap::new();
    for _ in 0..2 {
        let sample = tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
            .await?
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let wire = WireSample::from_zenoh(sample)?;
        received.insert(wire.metadata().command_id(), wire);
    }
    assert_eq!(
        ReadResponse::decode(received[&1].payload())?.value,
        42,
        "active query retains the old accepted view"
    );
    assert_eq!(
        received[&2].metadata().wire_control()?,
        transport::WireControl::Busy
    );
    assert_eq!(
        calls.load(Ordering::Acquire),
        1,
        "busy request cannot enter the handler"
    );
    send_external_read(&bus, 3, u32::MAX).await?;
    let oversized = WireSample::from_zenoh(
        tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
            .await?
            .map_err(|e| anyhow::anyhow!(e.to_string()))?,
    )?;
    assert_eq!(
        oversized.metadata().wire_control()?,
        transport::WireControl::Oversized
    );
    assert!(oversized.payload().is_empty());
    send_external_read(&bus, 4, 1).await?;
    let refreshed = WireSample::from_zenoh(
        tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
            .await?
            .map_err(|e| anyhow::anyhow!(e.to_string()))?,
    )?;
    assert_eq!(
        ReadResponse::decode(refreshed.payload())?.value,
        43,
        "later queries see newly accepted State"
    );
    send_external_read(&bus, 5, 1).await?;
    wait_entered(4).await?;
    runner.reset(ExecutionTime::default(), ())?;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), replies.recv_async())
            .await
            .is_err(),
        "retired view cannot publish after reset"
    );
    send_external_read(&bus, 6, 1).await?;
    let reset = WireSample::from_zenoh(
        tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
            .await?
            .map_err(|e| anyhow::anyhow!(e.to_string()))?,
    )?;
    assert_eq!(ReadResponse::decode(reset.payload())?.value, 42);
    send_external_read(&bus, 7, 1).await?;
    wait_entered(6).await?;
    runner.stop()?;
    owner.close().await;
    assert!(
        !matches!(replies.try_recv(), Ok(Some(_))),
        "stop joins the worker without publishing its retired result"
    );
    Ok(())
}
