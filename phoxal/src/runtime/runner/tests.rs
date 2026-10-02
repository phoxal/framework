use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
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

    fn retain_artifact_metadata() {}
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
        requirement_destinations: BTreeMap::new(),
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
        "executables": [{
            "instance": "sensor",
            "path": "bin/sensor"
        }]
    });
    std::fs::write(
        temporary.join("manifest.json"),
        serde_json::to_vec(&manifest).expect("manifest serializes"),
    )
    .expect("manifest writes");
    std::fs::write(
        temporary.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot:\n  id: driver-fixture\n  components:\n    sensor:\n      component: hardware-driver-fixture\n      mount_site: sensor_mount\n      driver:\n        config:\n          value: 42\nservices: {}\nconnections: {}\n",
    )
    .expect("compiled robot document writes");

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

#[crate::message(package = "phoxal.runtime.test")]
struct TransportRequest {
    #[phoxal(tag = 1)]
    value: u32,
}

#[crate::message(package = "phoxal.runtime.test")]
struct TransportResponse {
    #[phoxal(tag = 1)]
    value: u32,
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
        _value: Option<crate::runtime::input::SetpointUpdate>,
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
        _provenance: Option<crate::runtime::ObservationStamp>,
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

    fn retain_artifact_metadata() {}
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
struct RequestClientInputs {
    #[crate::runtime::input(max_response_bytes = 64)]
    request: crate::runtime::Request<u64, TransportRequest, TransportResponse>,
}

type RequestObservation = (
    crate::runtime::ReadStatus,
    bool,
    Option<Result<u32, RequestError>>,
);

struct RequestClientRuntime {
    selected_key: Arc<Mutex<Option<u64>>>,
    observations: Arc<Mutex<Vec<RequestObservation>>>,
}

impl Runtime for RequestClientRuntime {
    type Config = ();
    type State = ();
    type Inputs = RequestClientInputs;
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
        let completion = inputs.request.new_completion().map(|completion| {
            completion
                .result()
                .map(|response| response.value)
                .map_err(Clone::clone)
        });
        self.observations.lock().unwrap().push((
            inputs.request.status(),
            completion.is_some(),
            completion,
        ));
        Ok((state, ()))
    }
}

impl RegisteredRuntime for RequestClientRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(1, 100, 100);

    fn retain_artifact_metadata() {}
}

#[crate::runtime::outputs]
impl RequestClientRuntime {
    #[crate::runtime::outputs::activate(request, timeout_ms = 25)]
    fn request(&self, _state: &()) -> Option<crate::runtime::Activation<u64, TransportRequest>> {
        self.selected_key
            .lock()
            .unwrap()
            .map(|key| crate::runtime::Activation::new(key, TransportRequest { value: 41 }))
    }
}

fn request_client_manifest() -> RuntimeLaunchManifest {
    let signature = SourceMethodSignature {
        endpoint: TRANSPORT_PORT.name.to_owned(),
        service: TRANSPORT_PORT.service.to_owned(),
        method: TRANSPORT_PORT.method.to_owned(),
        shape: crate::artifact::MethodShape::Call,
        request: TRANSPORT_PORT.request.to_owned(),
        response: TRANSPORT_PORT.response.to_owned(),
        retained_latest: false,
        lease_valid_for_ms: None,
    };
    RuntimeLaunchManifest {
        root: PathBuf::from("."),
        robot_id: "typed-request-test".to_owned(),
        instance_id: "request-client".to_owned(),
        executable: PathBuf::from("typed-request-test"),
        executable_sha256: "00".repeat(32),
        config: Value::Object(serde_json::Map::new()),
        connections: BTreeMap::from([(
            "request-client.request".to_owned(),
            vec!["server.transport-commands".to_owned()],
        )]),
        requirement_destinations: BTreeMap::new(),
        artifacts: BTreeMap::from([(
            "server".to_owned(),
            SourceRuntimeRecord {
                period_ms: Some(1),
                timeout_ms: Some(100),
                init_timeout_ms: Some(100),
                inputs: vec![SourceInputRecord {
                    name: "commands".to_owned(),
                    role: "call_ingress".to_owned(),
                    max_items: Some(4),
                    max_bytes: Some(1024),
                    port: Some(TRANSPORT_PORT.name.to_owned()),
                    signature: Some(signature),
                }],
                transient_outputs: vec![SourceOutputRecord {
                    name: "replies".to_owned(),
                    role: "reply".to_owned(),
                    port: Some(TRANSPORT_PORT.name.to_owned()),
                    signature: None,
                    input: Some("commands".to_owned()),
                    max_items: Some(4),
                    max_bytes: Some(1024),
                    max_request_bytes: None,
                }],
                service_outputs: Vec::new(),
            },
        )]),
        scenario_producers: BTreeMap::new(),
        observation_providers: BTreeMap::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_request_is_one_shot_across_reply_timeout_withdrawal_and_reset()
-> crate::Result<()> {
    use zenoh::bytes::Encoding;

    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("request-client")?,
            Vec::new(),
        ),
    )
    .await?;
    let manifest = request_client_manifest();
    let correlations = Arc::new(Mutex::new(BTreeMap::new()));
    let expired = Arc::new(Mutex::new(BTreeSet::new()));
    let operations = Arc::new(Mutex::new(Vec::new()));
    let exchanges = Arc::new(Mutex::new(Vec::new()));
    let activations = Arc::new(Mutex::new(BTreeMap::new()));
    let mut input = ExecutionInputAdapter::<RequestClientRuntime>::unbound().with_shared_state(
        Arc::clone(&correlations),
        Arc::clone(&expired),
        Arc::clone(&operations),
        Arc::clone(&exchanges),
        Arc::clone(&activations),
    );
    input.bind(bus.clone(), &manifest).await?;
    let mut output = ExecutionOutputAdapter::<RequestClientRuntime>::unbound().with_shared_state(
        correlations,
        expired,
        operations,
        exchanges,
        activations,
    );
    output
        .bind(bus.clone(), &manifest.instance_id, &manifest)
        .await?;
    let selected_key = Arc::new(Mutex::new(Some(9)));
    let observations = Arc::new(Mutex::new(Vec::new()));
    let mut runner = RuntimeRunner::new(
        RequestClientRuntime {
            selected_key: Arc::clone(&selected_key),
            observations: Arc::clone(&observations),
        },
        ExecutionTime::default(),
        (),
        input,
        output,
    )?;
    let requests = bus
        .session()?
        .declare_subscriber(bus.full_key(&transport::port_key(
            "server",
            TRANSPORT_PORT.name,
            "request",
        )))
        .with(zenoh::handlers::FifoChannel::new(4))
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;

    runner.poll(ExecutionTime::default())?;
    let first = WireSample::from_zenoh(
        tokio::time::timeout(Duration::from_secs(2), requests.recv_async())
            .await?
            .map_err(|error| anyhow::anyhow!(error.to_string()))?,
    )?;
    assert_eq!(TransportRequest::decode(first.payload())?.value, 41);
    runner.poll(ExecutionTime::from_nanos(1_000_000))?;
    assert!(
        requests
            .try_recv()
            .expect("request queue remains readable")
            .is_none(),
        "pending key must not resend"
    );

    let metadata = first.metadata();
    let reply_metadata = transport::reply_metadata(
        "server",
        StepContext::first(
            ExecutionTime::from_nanos(2_000_000),
            ExecutionDuration::from_millis(1),
        ),
        metadata.command_id(),
        metadata.eligible_boundary(),
        metadata.caller_rank.expect("request caller rank"),
    )
    .with_caller(metadata.caller.clone().expect("request caller"))
    .encode_bounded()?;
    bus.session()?
        .put(
            bus.full_key(&transport::port_key("server", TRANSPORT_PORT.name, "reply")),
            transport::encode_prost(&TransportResponse { value: 42 })?,
        )
        .encoding(Encoding::from(transport::PROTOBUF_ENCODING.to_owned()))
        .attachment(reply_metadata)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    tokio::time::sleep(Duration::from_millis(10)).await;
    runner.poll(ExecutionTime::from_nanos(2_000_000))?;
    runner.poll(ExecutionTime::from_nanos(3_000_000))?;
    assert_eq!(
        observations.lock().unwrap().as_slice(),
        &[
            (crate::runtime::ReadStatus::Inactive, false, None),
            (crate::runtime::ReadStatus::Pending, false, None),
            (crate::runtime::ReadStatus::Completed, true, Some(Ok(42))),
            (crate::runtime::ReadStatus::Completed, false, None),
        ]
    );
    assert!(
        requests
            .try_recv()
            .expect("request queue remains readable")
            .is_none(),
        "completed key must not resend"
    );

    *selected_key.lock().unwrap() = None;
    runner.poll(ExecutionTime::from_nanos(4_000_000))?;
    runner.poll(ExecutionTime::from_nanos(5_000_000))?;
    assert_eq!(
        observations.lock().unwrap().last(),
        Some(&(crate::runtime::ReadStatus::Inactive, false, None))
    );

    *selected_key.lock().unwrap() = Some(9);
    runner.poll(ExecutionTime::from_nanos(6_000_000))?;
    let retry = WireSample::from_zenoh(
        tokio::time::timeout(Duration::from_secs(2), requests.recv_async())
            .await?
            .map_err(|error| anyhow::anyhow!(error.to_string()))?,
    )?;
    assert_ne!(retry.metadata().command_id, first.metadata().command_id);
    tokio::time::sleep(Duration::from_millis(35)).await;
    runner.poll(ExecutionTime::from_nanos(7_000_000))?;
    runner.poll(ExecutionTime::from_nanos(8_000_000))?;
    assert!(matches!(
        observations.lock().unwrap().as_slice(),
        [
            ..,
            (
                crate::runtime::ReadStatus::Completed,
                true,
                Some(Err(RequestError::OutcomeUnknown(_)))
            ),
            (crate::runtime::ReadStatus::Completed, false, None)
        ]
    ));
    assert!(
        requests
            .try_recv()
            .expect("request queue remains readable")
            .is_none(),
        "unknown outcome must not retry implicitly"
    );

    let retry_metadata = retry.metadata();
    let late_reply_metadata = transport::reply_metadata(
        "server",
        StepContext::first(
            ExecutionTime::from_nanos(9_000_000),
            ExecutionDuration::from_millis(1),
        ),
        retry_metadata.command_id(),
        retry_metadata.eligible_boundary(),
        retry_metadata
            .caller_rank
            .expect("late request caller rank"),
    )
    .with_caller(retry_metadata.caller.clone().expect("late request caller"))
    .encode_bounded()?;
    bus.session()?
        .put(
            bus.full_key(&transport::port_key("server", TRANSPORT_PORT.name, "reply")),
            transport::encode_prost(&TransportResponse { value: 99 })?,
        )
        .encoding(Encoding::from(transport::PROTOBUF_ENCODING.to_owned()))
        .attachment(late_reply_metadata)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    tokio::time::sleep(Duration::from_millis(10)).await;
    runner.poll(ExecutionTime::from_nanos(9_000_000))?;
    assert_eq!(
        observations.lock().unwrap().last(),
        Some(&(crate::runtime::ReadStatus::Completed, false, None)),
        "a late reply cannot publish a second completion"
    );

    runner.reset(ExecutionTime::from_nanos(10_000_000), ())?;
    *selected_key.lock().unwrap() = None;
    runner.poll(ExecutionTime::from_nanos(10_000_000))?;
    assert_eq!(
        observations.lock().unwrap().last(),
        Some(&(crate::runtime::ReadStatus::Inactive, false, None))
    );
    runner.stop()?;
    owner.close().await;
    Ok(())
}

const GENERATED_TRANSPORT_METHOD: crate::contracts::CallMethod<
    TransportRequest,
    TransportResponse,
> = crate::contracts::CallMethod::new(
    "phoxal.runtime.test",
    "Transport",
    "transport-commands",
    "phoxal.runtime.test.TransportRequest",
    "phoxal.runtime.test.TransportResponse",
    None,
    &[],
);

#[crate::runtime::inputs]
struct GeneratedCallInputs {
    completions: crate::runtime::Completions,
}

#[derive(Default)]
struct GeneratedCallState {
    ticket: Option<crate::runtime::outputs::CallTicket<TransportResponse>>,
}

struct GeneratedCallRuntime {
    response: Arc<Mutex<Option<u32>>>,
}

impl Runtime for GeneratedCallRuntime {
    type Config = ();
    type State = GeneratedCallState;
    type Inputs = GeneratedCallInputs;
    type Outputs = crate::runtime::Outputs;

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
        Ok(GeneratedCallState::default())
    }

    fn step(
        &self,
        ctx: &StepContext,
        mut state: Self::State,
        inputs: &Self::Inputs,
    ) -> crate::Result<(Self::State, Self::Outputs)> {
        let mut outputs = crate::runtime::Outputs::default();
        if let Some(ticket) = state.ticket {
            if let Some(completion) = inputs.completions.get(&ticket) {
                let response = completion.into_result()?;
                *self.response.lock().expect("generated response lock") = Some(response.value);
                state.ticket = None;
            }
        } else if self
            .response
            .lock()
            .expect("generated response lock")
            .is_none()
        {
            state.ticket = Some(outputs.send(
                ctx,
                GENERATED_TRANSPORT_METHOD.bind("server", TransportRequest { value: 41 }),
            )?);
        }
        Ok((state, outputs))
    }
}

impl RegisteredRuntime for GeneratedCallRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(1, 100, 100);

    fn retain_artifact_metadata() {}
}

impl crate::runtime::outputs::OutputBindings for GeneratedCallRuntime {
    const FIELDS: &'static [crate::runtime::outputs::OutputField] = &[];
}

fn generated_call_manifest() -> RuntimeLaunchManifest {
    let signature = SourceMethodSignature {
        endpoint: TRANSPORT_PORT.name.to_owned(),
        service: TRANSPORT_PORT.service.to_owned(),
        method: TRANSPORT_PORT.method.to_owned(),
        shape: crate::artifact::MethodShape::Call,
        request: TRANSPORT_PORT.request.to_owned(),
        response: TRANSPORT_PORT.response.to_owned(),
        retained_latest: false,
        lease_valid_for_ms: None,
    };
    let caller = SourceRuntimeRecord {
        period_ms: Some(1),
        timeout_ms: Some(100),
        init_timeout_ms: Some(100),
        inputs: vec![SourceInputRecord {
            name: "completions".to_owned(),
            role: "call_completions".to_owned(),
            max_items: None,
            max_bytes: None,
            port: None,
            signature: None,
        }],
        transient_outputs: Vec::new(),
        service_outputs: Vec::new(),
    };
    let server = SourceRuntimeRecord {
        period_ms: Some(1),
        timeout_ms: Some(100),
        init_timeout_ms: Some(100),
        inputs: vec![SourceInputRecord {
            name: "commands".to_owned(),
            role: "call_ingress".to_owned(),
            max_items: Some(4),
            max_bytes: Some(1024),
            port: Some(TRANSPORT_PORT.name.to_owned()),
            signature: Some(signature),
        }],
        transient_outputs: vec![SourceOutputRecord {
            name: "replies".to_owned(),
            role: "reply".to_owned(),
            port: Some(TRANSPORT_PORT.name.to_owned()),
            signature: None,
            input: Some("commands".to_owned()),
            max_items: Some(4),
            max_bytes: Some(1024),
            max_request_bytes: None,
        }],
        service_outputs: Vec::new(),
    };
    RuntimeLaunchManifest {
        root: PathBuf::from("."),
        robot_id: "generated-call-test".to_owned(),
        instance_id: "caller".to_owned(),
        executable: PathBuf::from("generated-call-test"),
        executable_sha256: "00".repeat(32),
        config: Value::Object(serde_json::Map::new()),
        connections: BTreeMap::new(),
        requirement_destinations: BTreeMap::new(),
        artifacts: BTreeMap::from([("caller".to_owned(), caller), ("server".to_owned(), server)]),
        scenario_producers: BTreeMap::new(),
        observation_providers: BTreeMap::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_call_crosses_transport_and_completes_in_a_later_invocation() -> crate::Result<()>
{
    use zenoh::bytes::Encoding;

    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("generated-call-client")?,
            Vec::new(),
        ),
    )
    .await?;
    let manifest = generated_call_manifest();
    let generated_correlations = Arc::new(Mutex::new(BTreeMap::new()));
    let generated_completions = Arc::new(Mutex::new(Vec::new()));
    let mut input = ExecutionInputAdapter::<GeneratedCallRuntime>::unbound().with_generated_calls(
        Arc::clone(&generated_correlations),
        Arc::clone(&generated_completions),
    );
    input.bind(bus.clone(), &manifest).await?;
    let mut output = ExecutionOutputAdapter::<GeneratedCallRuntime>::unbound()
        .with_generated_calls(generated_correlations, generated_completions);
    output
        .bind(bus.clone(), &manifest.instance_id, &manifest)
        .await?;
    let requests = bus
        .session()?
        .declare_subscriber(bus.full_key(&transport::port_key(
            "server",
            TRANSPORT_PORT.name,
            "request",
        )))
        .with(zenoh::handlers::FifoChannel::new(4))
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let response = Arc::new(Mutex::new(None));
    let mut runner = RuntimeRunner::new(
        GeneratedCallRuntime {
            response: Arc::clone(&response),
        },
        ExecutionTime::default(),
        (),
        input,
        output,
    )?;

    runner.poll(ExecutionTime::default())?;
    let request = WireSample::from_zenoh(
        tokio::time::timeout(Duration::from_secs(2), requests.recv_async())
            .await?
            .map_err(|error| anyhow::anyhow!(error.to_string()))?,
    )?;
    assert_eq!(TransportRequest::decode(request.payload())?.value, 41);
    assert_eq!(*response.lock().expect("generated response lock"), None);

    let request_metadata = request.metadata();
    let reply_metadata = transport::reply_metadata(
        "server",
        StepContext::first(
            ExecutionTime::from_nanos(1_000_000),
            ExecutionDuration::from_millis(1),
        ),
        request_metadata.command_id(),
        request_metadata.eligible_boundary(),
        request_metadata
            .caller_rank
            .expect("generated request caller rank"),
    )
    .with_caller(
        request_metadata
            .caller
            .clone()
            .expect("generated request caller"),
    )
    .encode_bounded()?;
    bus.session()?
        .put(
            bus.full_key(&transport::port_key("server", TRANSPORT_PORT.name, "reply")),
            transport::encode_prost(&TransportResponse { value: 42 })?,
        )
        .encoding(Encoding::from(transport::PROTOBUF_ENCODING.to_owned()))
        .attachment(reply_metadata)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    tokio::time::sleep(Duration::from_millis(10)).await;
    runner.poll(ExecutionTime::from_nanos(1_000_000))?;

    assert_eq!(*response.lock().expect("generated response lock"), Some(42));
    assert!(
        requests
            .try_recv()
            .expect("generated request queue remains readable")
            .is_none(),
        "a completed generated call must not be replayed"
    );

    runner.reset(ExecutionTime::from_nanos(2_000_000), ())?;
    *response.lock().expect("generated response lock") = None;
    runner.poll(ExecutionTime::from_nanos(2_000_000))?;
    let replacement = WireSample::from_zenoh(
        tokio::time::timeout(Duration::from_secs(2), requests.recv_async())
            .await?
            .map_err(|error| anyhow::anyhow!(error.to_string()))?,
    )?;
    assert_ne!(
        replacement.metadata().command_id,
        request_metadata.command_id,
        "wire correlation identities must not be reused across reset"
    );

    bus.session()?
        .put(
            bus.full_key(&transport::port_key("server", TRANSPORT_PORT.name, "reply")),
            transport::encode_prost(&TransportResponse { value: 99 })?,
        )
        .encoding(Encoding::from(transport::PROTOBUF_ENCODING.to_owned()))
        .attachment(
            transport::reply_metadata(
                "server",
                StepContext::first(
                    ExecutionTime::from_nanos(3_000_000),
                    ExecutionDuration::from_millis(1),
                ),
                request_metadata.command_id(),
                request_metadata.eligible_boundary(),
                request_metadata
                    .caller_rank
                    .expect("stale generated request caller rank"),
            )
            .with_caller(
                request_metadata
                    .caller
                    .clone()
                    .expect("stale generated request caller"),
            )
            .encode_bounded()?,
        )
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    tokio::time::sleep(Duration::from_millis(10)).await;
    runner.poll(ExecutionTime::from_nanos(3_000_000))?;
    assert_eq!(
        *response.lock().expect("generated response lock"),
        None,
        "a pre-reset reply must not complete the replacement call"
    );

    let replacement_metadata = replacement.metadata();
    bus.session()?
        .put(
            bus.full_key(&transport::port_key("server", TRANSPORT_PORT.name, "reply")),
            transport::encode_prost(&TransportResponse { value: 43 })?,
        )
        .encoding(Encoding::from(transport::PROTOBUF_ENCODING.to_owned()))
        .attachment(
            transport::reply_metadata(
                "server",
                StepContext::first(
                    ExecutionTime::from_nanos(4_000_000),
                    ExecutionDuration::from_millis(1),
                ),
                replacement_metadata.command_id(),
                replacement_metadata.eligible_boundary(),
                replacement_metadata
                    .caller_rank
                    .expect("replacement generated request caller rank"),
            )
            .with_caller(
                replacement_metadata
                    .caller
                    .clone()
                    .expect("replacement generated request caller"),
            )
            .encode_bounded()?,
        )
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    tokio::time::sleep(Duration::from_millis(10)).await;
    runner.poll(ExecutionTime::from_nanos(4_000_000))?;
    assert_eq!(*response.lock().expect("generated response lock"), Some(43));
    runner.stop()?;
    owner.close().await;
    Ok(())
}

#[crate::runtime::inputs]
struct LocalRequirementInputs {
    // Two fields deliberately declare the SAME descriptor: each keeps its
    // own connection, destination, and completion copy, proving per-field
    // requirement identity instead of descriptor-keyed collapsing. The
    // field names differ from the served port name on purpose.
    ask: crate::runtime::Completions,
    verify: crate::runtime::Completions,
}

#[derive(Default)]
struct LocalRequirementState {
    ask: Option<crate::runtime::outputs::CallTicket<TransportResponse>>,
    verify: Option<crate::runtime::outputs::CallTicket<TransportResponse>>,
}

struct LocalRequirementRuntime {
    response: Arc<Mutex<Option<u32>>>,
    verification: Arc<Mutex<Option<u32>>>,
}

impl Runtime for LocalRequirementRuntime {
    type Config = ();
    type State = LocalRequirementState;
    type Inputs = LocalRequirementInputs;
    type Outputs = crate::runtime::Outputs;

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
        Ok(LocalRequirementState::default())
    }

    fn step(
        &self,
        ctx: &StepContext,
        mut state: Self::State,
        inputs: &Self::Inputs,
    ) -> crate::Result<(Self::State, Self::Outputs)> {
        let mut outputs = crate::runtime::Outputs::default();
        if let Some(ticket) = state.ask
            && let Some(completion) = inputs.ask.take(&ticket)
        {
            let response = completion.into_result()?;
            *self.response.lock().expect("local requirement lock") = Some(response.value);
            state.ask = None;
        } else if state.ask.is_none() {
            // The generated constructors' staging shape: the call carries
            // its own field's name through routing.
            state.ask = Some(outputs.send(
                ctx,
                GENERATED_TRANSPORT_METHOD.bind("ask", TransportRequest { value: 41 }),
            )?);
        }
        // Completions live in the single retained store (the first call
        // field), regardless of which field's call produced them, and are
        // consumed destructively so the retained mailbox reclaims them.
        if let Some(ticket) = state.verify
            && let Some(completion) = inputs.ask.take(&ticket)
        {
            let response = completion.into_result()?;
            *self.verification.lock().expect("verification lock") = Some(response.value);
            state.verify = None;
        } else if state.verify.is_none() {
            state.verify = Some(outputs.send(
                ctx,
                GENERATED_TRANSPORT_METHOD.bind("verify", TransportRequest { value: 41 }),
            )?);
        }
        Ok((state, outputs))
    }
}

impl RegisteredRuntime for LocalRequirementRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(1, 100, 100);

    fn retain_artifact_metadata() {}
}

impl crate::runtime::outputs::OutputBindings for LocalRequirementRuntime {
    const FIELDS: &'static [crate::runtime::outputs::OutputField] = &[];
}

#[derive(Clone, Default)]
struct CountingProviderRuntime {
    handled: Arc<std::sync::atomic::AtomicUsize>,
    offset: u32,
}

impl Runtime for CountingProviderRuntime {
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
            self.handled
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            outputs.replies.push(command.reply(TransportResponse {
                value: command.request().value.saturating_add(self.offset),
            }));
        }
        Ok((state, outputs))
    }
}

impl RegisteredRuntime for CountingProviderRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(1, 100, 100);

    fn retain_artifact_metadata() {}
}

impl crate::runtime::outputs::OutputBindings for CountingProviderRuntime {
    const FIELDS: &'static [crate::runtime::outputs::OutputField] = &[];
}

fn local_requirement_manifest() -> RuntimeLaunchManifest {
    let mut manifest = generated_call_manifest();
    let signature = SourceMethodSignature {
        endpoint: TRANSPORT_PORT.name.to_owned(),
        service: TRANSPORT_PORT.service.to_owned(),
        method: TRANSPORT_PORT.method.to_owned(),
        shape: crate::artifact::MethodShape::Call,
        request: TRANSPORT_PORT.request.to_owned(),
        response: TRANSPORT_PORT.response.to_owned(),
        retained_latest: false,
        lease_valid_for_ms: None,
    };
    let requirement = |field: &str| SourceInputRecord {
        name: field.to_owned(),
        role: "call_completions".to_owned(),
        max_items: None,
        max_bytes: None,
        port: Some(TRANSPORT_PORT.name.to_owned()),
        signature: Some(signature.clone()),
    };
    let consumer = SourceRuntimeRecord {
        period_ms: Some(1),
        timeout_ms: Some(100),
        init_timeout_ms: Some(100),
        inputs: vec![requirement("ask"), requirement("verify")],
        transient_outputs: Vec::new(),
        service_outputs: Vec::new(),
    };
    manifest.instance_id = "local".to_owned();
    manifest.connections = BTreeMap::from([
        (
            "local.ask".to_owned(),
            vec![format!("countdown.{}", TRANSPORT_PORT.name)],
        ),
        (
            "local.verify".to_owned(),
            vec![format!("countdown2.{}", TRANSPORT_PORT.name)],
        ),
    ]);
    // Bundle admission resolves each requirement destination once, keyed by
    // the local field name, exactly as precompute_requirement_destinations
    // would for an admitted bundle.
    manifest.requirement_destinations = BTreeMap::from([
        ("ask".to_owned(), ("countdown".to_owned(), TRANSPORT_PORT)),
        (
            "verify".to_owned(),
            ("countdown2".to_owned(), TRANSPORT_PORT),
        ),
    ]);
    manifest.artifacts.insert("local".to_owned(), consumer);
    let server = manifest
        .artifacts
        .remove("server")
        .expect("the shared server record exists");
    manifest
        .artifacts
        .insert("countdown".to_owned(), server.clone());
    manifest.artifacts.insert("countdown2".to_owned(), server);
    manifest
}

fn poll_provider(
    runner: &mut Option<
        RuntimeRunner<
            CountingProviderRuntime,
            ExecutionInputAdapter<CountingProviderRuntime>,
            ExecutionOutputAdapter<CountingProviderRuntime>,
        >,
    >,
    attempt: u64,
) -> crate::Result<()> {
    if let Some(runner) = runner {
        runner.poll(ExecutionTime::from_nanos(attempt * 1_000_000))?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_local_requirement_resolves_through_the_graph_and_completes() -> crate::Result<()>
{
    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("local-requirement-graph")?,
            Vec::new(),
        ),
    )
    .await?;
    let consumer_manifest = local_requirement_manifest();
    let generated_correlations = Arc::new(Mutex::new(BTreeMap::new()));
    let generated_completions = Arc::new(Mutex::new(Vec::new()));
    let mut input = ExecutionInputAdapter::<LocalRequirementRuntime>::unbound()
        .with_generated_calls(
            Arc::clone(&generated_correlations),
            Arc::clone(&generated_completions),
        );
    input.bind(bus.clone(), &consumer_manifest).await?;
    let mut output = ExecutionOutputAdapter::<LocalRequirementRuntime>::unbound()
        .with_generated_calls(generated_correlations, generated_completions);
    output
        .bind(
            bus.clone(),
            &consumer_manifest.instance_id,
            &consumer_manifest,
        )
        .await?;
    let response = Arc::new(Mutex::new(None));
    let verification = Arc::new(Mutex::new(None));
    let mut consumer = RuntimeRunner::new(
        LocalRequirementRuntime {
            response: Arc::clone(&response),
            verification: Arc::clone(&verification),
        },
        ExecutionTime::default(),
        (),
        input,
        output,
    )?;

    // Both provider sides are real runners over the same execution and the
    // same shared bundle graph, each serving the same descriptor with its
    // own response offset, so a crossed destination is observable.
    let ask_handled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let verify_handled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut providers = Vec::new();
    for (instance, handled, offset) in [
        ("countdown", Arc::clone(&ask_handled), 1),
        ("countdown2", Arc::clone(&verify_handled), 2),
    ] {
        let mut manifest = consumer_manifest.clone();
        manifest.instance_id = instance.to_owned();
        let mut provider_input = ExecutionInputAdapter::<CountingProviderRuntime>::unbound();
        let mut provider_output = ExecutionOutputAdapter::<CountingProviderRuntime>::unbound();
        provider_input.bind(bus.clone(), &manifest).await?;
        provider_output
            .bind(bus.clone(), &manifest.instance_id, &manifest)
            .await?;
        providers.push(RuntimeRunner::new(
            CountingProviderRuntime { handled, offset },
            ExecutionTime::default(),
            (),
            provider_input,
            provider_output,
        )?);
    }
    let mut providers = providers.into_iter();
    let (mut first, mut second) = (providers.next(), providers.next());

    consumer.poll(ExecutionTime::default())?;
    assert_eq!(*response.lock().expect("local requirement lock"), None);
    let mut completed = false;
    for attempt in 1_u64..40 {
        poll_provider(&mut first, attempt)?;
        poll_provider(&mut second, attempt)?;
        consumer.poll(ExecutionTime::from_nanos(attempt * 1_000_000 + 500_000))?;
        if response.lock().expect("local requirement lock").is_some()
            && verification.lock().expect("verification lock").is_some()
        {
            completed = true;
            break;
        }
        assert!(
            attempt < 39,
            "the served requirements never completed: ask handled {}, verify handled {}, status {:?}",
            ask_handled.load(std::sync::atomic::Ordering::Relaxed),
            verify_handled.load(std::sync::atomic::Ordering::Relaxed),
            consumer.status(),
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(completed);
    assert_eq!(
        ask_handled.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the ask field's request is admitted by its own provider exactly once"
    );
    assert_eq!(
        verify_handled.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the verify field's request is admitted by its own provider exactly once"
    );

    assert_eq!(
        *response.lock().expect("local requirement lock"),
        Some(42),
        "the ask field completes through its own served port"
    );
    assert_eq!(
        *verification.lock().expect("verification lock"),
        Some(43),
        "the verify field completes through its own, different provider"
    );

    consumer.stop()?;
    if let Some(mut runner) = first {
        runner.stop()?;
    }
    if let Some(mut runner) = second {
        runner.stop()?;
    }
    owner.close().await;
    Ok(())
}

// Real-time soak over a live connection: rounds settle through real
// delivery, so it runs in the integration lane (`e2e` feature).
#[cfg(feature = "e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn five_hundred_rounds_of_two_field_calls_drain_the_retained_mailbox() -> crate::Result<()> {
    // A one-shot counter never reaches a retained-storage leak: this drain
    // cycles both call fields through the real exchange and retained
    // mailbox 512 times, so a consumed result that failed to release its
    // item or byte charge exhausts the mailbox visibly.
    const ROUNDS: usize = 512;
    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("drain-graph")?,
            Vec::new(),
        ),
    )
    .await?;
    let consumer_manifest = local_requirement_manifest();
    let generated_correlations = Arc::new(Mutex::new(BTreeMap::new()));
    let generated_completions = Arc::new(Mutex::new(Vec::new()));
    let mut input = ExecutionInputAdapter::<LocalRequirementRuntime>::unbound()
        .with_generated_calls(
            Arc::clone(&generated_correlations),
            Arc::clone(&generated_completions),
        );
    input.bind(bus.clone(), &consumer_manifest).await?;
    let mut output = ExecutionOutputAdapter::<LocalRequirementRuntime>::unbound()
        .with_generated_calls(generated_correlations, generated_completions);
    output
        .bind(
            bus.clone(),
            &consumer_manifest.instance_id,
            &consumer_manifest,
        )
        .await?;
    let response = Arc::new(Mutex::new(None));
    let verification = Arc::new(Mutex::new(None));
    let mut consumer = RuntimeRunner::new(
        LocalRequirementRuntime {
            response: Arc::clone(&response),
            verification: Arc::clone(&verification),
        },
        ExecutionTime::default(),
        (),
        input,
        output,
    )?;

    let ask_handled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let verify_handled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut providers = Vec::new();
    for (instance, handled, offset) in [
        ("countdown", Arc::clone(&ask_handled), 1),
        ("countdown2", Arc::clone(&verify_handled), 2),
    ] {
        let mut manifest = consumer_manifest.clone();
        manifest.instance_id = instance.to_owned();
        let mut provider_input = ExecutionInputAdapter::<CountingProviderRuntime>::unbound();
        let mut provider_output = ExecutionOutputAdapter::<CountingProviderRuntime>::unbound();
        provider_input.bind(bus.clone(), &manifest).await?;
        provider_output
            .bind(bus.clone(), &manifest.instance_id, &manifest)
            .await?;
        providers.push(RuntimeRunner::new(
            CountingProviderRuntime { handled, offset },
            ExecutionTime::default(),
            (),
            provider_input,
            provider_output,
        )?);
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut tick = 0_u64;
    loop {
        tick = tick.saturating_add(1);
        for provider in &mut providers {
            provider.poll(ExecutionTime::from_nanos(tick * 1_000_000))?;
        }
        consumer.poll(ExecutionTime::from_nanos(tick * 1_000_000 + 500_000))?;
        let ask = ask_handled.load(std::sync::atomic::Ordering::Relaxed);
        let verify = verify_handled.load(std::sync::atomic::Ordering::Relaxed);
        if ask >= ROUNDS && verify >= ROUNDS {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the drain stalled after ask {ask}/{ROUNDS} and verify {verify}/{ROUNDS}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    // Every consumed result released its retained item and byte charge:
    // the mailbox is empty and the drain never exhausted a bound.
    consumer.stop()?;
    for provider in providers {
        let mut provider = provider;
        provider.stop()?;
    }
    owner.close().await;
    crate::Result::Ok(())
}

#[crate::runtime::inputs]
struct OutstandingCallInputs {
    completions: crate::runtime::Completions,
}

#[derive(Default)]
struct OutstandingCallState {
    tickets: Vec<crate::runtime::outputs::CallTicket<TransportResponse>>,
}

struct OutstandingCallRuntime {
    responses: Arc<Mutex<Vec<u32>>>,
    stage: Arc<std::sync::atomic::AtomicBool>,
}

impl Runtime for OutstandingCallRuntime {
    type Config = ();
    type State = OutstandingCallState;
    type Inputs = OutstandingCallInputs;
    type Outputs = crate::runtime::Outputs;

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
        Ok(OutstandingCallState::default())
    }

    fn step(
        &self,
        ctx: &StepContext,
        mut state: Self::State,
        inputs: &Self::Inputs,
    ) -> crate::Result<(Self::State, Self::Outputs)> {
        let mut outputs = crate::runtime::Outputs::default();
        state
            .tickets
            .retain(|ticket| match inputs.completions.get(ticket) {
                Some(completion) => {
                    if let Ok(response) = completion.response() {
                        self.responses.lock().unwrap().push(response.value);
                    }
                    false
                }
                None => true,
            });
        if self.stage.load(std::sync::atomic::Ordering::Relaxed) {
            state.tickets.push(outputs.send(
                ctx,
                GENERATED_TRANSPORT_METHOD.bind("server", TransportRequest { value: 7 }),
            )?);
        }
        Ok((state, outputs))
    }
}

impl RegisteredRuntime for OutstandingCallRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(1, 100, 100);

    fn retain_artifact_metadata() {}
}

impl crate::runtime::outputs::OutputBindings for OutstandingCallRuntime {
    const FIELDS: &'static [crate::runtime::outputs::OutputField] = &[];
}

/// Cross-runtime flow control: the sender refuses to stage more calls than
/// the provider's declared ingress bound while earlier calls are still
/// outstanding, and recovers once their completions retire the reservations.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_call_outstanding_cap_enforces_provider_ingress_bound() -> crate::Result<()> {
    use std::sync::atomic::Ordering;

    use zenoh::bytes::Encoding;

    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("outstanding-call-client")?,
            Vec::new(),
        ),
    )
    .await?;
    let manifest = generated_call_manifest();
    let generated_correlations = Arc::new(Mutex::new(BTreeMap::new()));
    let generated_completions = Arc::new(Mutex::new(Vec::new()));
    let mut input = ExecutionInputAdapter::<OutstandingCallRuntime>::unbound()
        .with_generated_calls(
            Arc::clone(&generated_correlations),
            Arc::clone(&generated_completions),
        );
    input.bind(bus.clone(), &manifest).await?;
    let mut output = ExecutionOutputAdapter::<OutstandingCallRuntime>::unbound()
        .with_generated_calls(generated_correlations, generated_completions);
    output
        .bind(bus.clone(), &manifest.instance_id, &manifest)
        .await?;
    let requests = bus
        .session()?
        .declare_subscriber(bus.full_key(&transport::port_key(
            "server",
            TRANSPORT_PORT.name,
            "request",
        )))
        .with(zenoh::handlers::FifoChannel::new(8))
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let responses: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let stage = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut runner = RuntimeRunner::new(
        OutstandingCallRuntime {
            responses: Arc::clone(&responses),
            stage: Arc::clone(&stage),
        },
        ExecutionTime::default(),
        (),
        input,
        output,
    )?;

    // The provider declares max_items = 4: four polls stage four calls that
    // all reach the wire while nothing has replied yet.
    let mut request_metadata = Vec::new();
    for index in 0..4_u64 {
        let now = ExecutionTime::from_nanos((index + 1) * 1_000_000);
        runner.poll(now)?;
        let request = WireSample::from_zenoh(
            tokio::time::timeout(Duration::from_secs(2), requests.recv_async())
                .await?
                .map_err(|error| anyhow::anyhow!(error.to_string()))?,
        )?;
        request_metadata.push(request.metadata().clone());
    }

    // Retire the first two reservations with real replies.  Each reply
    // carries a distinct server boundary so per-rank fencing keeps both.
    for (index, metadata) in request_metadata.iter().take(2).enumerate() {
        bus.session()?
            .put(
                bus.full_key(&transport::port_key("server", TRANSPORT_PORT.name, "reply")),
                transport::encode_prost(&TransportResponse {
                    value: 100 + index as u32,
                })?,
            )
            .encoding(Encoding::from(transport::PROTOBUF_ENCODING.to_owned()))
            .attachment(
                transport::reply_metadata(
                    "server",
                    StepContext::first(
                        ExecutionTime::from_nanos((5 + index as u64) * 1_000_000),
                        ExecutionDuration::from_millis(1),
                    ),
                    metadata.command_id(),
                    metadata.eligible_boundary(),
                    metadata.caller_rank.expect("outstanding caller rank"),
                )
                .with_caller(metadata.caller.clone().expect("outstanding caller"))
                .encode_bounded()?,
            )
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    stage.store(false, Ordering::Relaxed);
    runner.poll(ExecutionTime::from_nanos(7_000_000))?;
    assert_eq!(
        responses.lock().unwrap().as_slice(),
        &[100, 101],
        "the two retired calls deliver completions"
    );

    // The cap counts outstanding calls, not lifetime sends: with two retired,
    // two more calls must be admitted without touching the provider.
    stage.store(true, Ordering::Relaxed);
    for index in 0..2_u64 {
        let now = ExecutionTime::from_nanos((8 + index) * 1_000_000);
        runner.poll(now)?;
        let request = WireSample::from_zenoh(
            tokio::time::timeout(Duration::from_secs(2), requests.recv_async())
                .await?
                .map_err(|error| anyhow::anyhow!(error.to_string()))?,
        )?;
        request_metadata.push(request.metadata().clone());
    }

    // A fifth outstanding call must be rejected at the sender before it
    // reaches the transport, naming the ingress bound.  The violation is
    // fatal for the runtime, which is the observable flow-control contract.
    let capped = runner.poll(ExecutionTime::from_nanos(10_000_000));
    assert!(
        capped
            .as_ref()
            .is_err_and(|error| error.to_string().contains("outstanding generated calls")),
        "the fifth outstanding call must be rejected at the sender, got {capped:?}"
    );
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        requests
            .try_recv()
            .expect("request queue remains readable")
            .is_none(),
        "the capped call must not reach the wire"
    );

    let _ = runner.stop();
    owner.close().await;
    Ok(())
}

#[crate::runtime::inputs]
struct OperationInputs {
    operation: crate::runtime::Operation<u64, u32>,
}

struct OperationRuntime {
    completion: Arc<Mutex<Option<u32>>>,
    observations: Arc<Mutex<Vec<(crate::runtime::ReadStatus, bool)>>>,
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
        self.observations.lock().unwrap().push((
            inputs.operation.status(),
            inputs.operation.new_completion().is_some(),
        ));
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

    fn retain_artifact_metadata() {}
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
    let activation_states = Arc::new(Mutex::new(BTreeMap::new()));
    let mut input = ExecutionInputAdapter::<OperationRuntime>::unbound().with_shared_state(
        Arc::clone(&correlations),
        Arc::clone(&expired_correlations),
        Arc::clone(&operation_completions),
        Arc::clone(&exchange_completions),
        Arc::clone(&activation_states),
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
        activation_states,
    );
    output.bind_direct(bus.clone(), "operation");
    let completion = Arc::new(Mutex::new(None));
    let observations = Arc::new(Mutex::new(Vec::new()));
    let mut runner = RuntimeRunner::new(
        OperationRuntime {
            completion: Arc::clone(&completion),
            observations: Arc::clone(&observations),
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
    let final_index = observations.lock().unwrap().len() as u64;
    runner.poll(ExecutionTime::from_nanos(final_index * 2_000_000))?;
    let observations = observations.lock().unwrap().clone();
    assert_eq!(
        observations.first(),
        Some(&(crate::runtime::ReadStatus::Inactive, false))
    );
    assert!(
        observations
            .iter()
            .any(|observation| { *observation == (crate::runtime::ReadStatus::Completed, true) })
    );
    assert_eq!(
        observations.last(),
        Some(&(crate::runtime::ReadStatus::Completed, false))
    );
    runner.stop().expect("runner stops");
    owner.close().await;
    Ok(())
}

static REPLACEMENT_OPERATION_ACTIVE: AtomicUsize = AtomicUsize::new(0);
static REPLACEMENT_OPERATION_MAX_ACTIVE: AtomicUsize = AtomicUsize::new(0);
static REPLACEMENT_OPERATION_STARTED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

type OperationTestCompletion = (u64, Result<u32, OperationInputError>);

struct ReplacementOperationRuntime {
    selected: Arc<Mutex<Option<(u64, u32)>>>,
    completions: Arc<Mutex<Vec<OperationTestCompletion>>>,
}

impl Runtime for ReplacementOperationRuntime {
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
            self.completions.lock().unwrap().push((
                *completion.key(),
                completion.result().copied().map_err(Clone::clone),
            ));
        }
        Ok((state, ()))
    }
}

impl RegisteredRuntime for ReplacementOperationRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(1, 100, 100);

    fn retain_artifact_metadata() {}
}

#[crate::runtime::outputs]
impl ReplacementOperationRuntime {
    #[crate::runtime::outputs::activate(operation)]
    fn activate(&self, _state: &()) -> Option<crate::runtime::Activation<u64, u32>> {
        self.selected
            .lock()
            .unwrap()
            .map(|(key, input)| crate::runtime::Activation::new(key, input))
    }

    #[crate::runtime::outputs::operation(operation, timeout_ms = 200, cancel_grace_ms = 100)]
    fn run(input: u32) -> crate::Result<u32> {
        REPLACEMENT_OPERATION_STARTED.lock().unwrap().push(input);
        let active = REPLACEMENT_OPERATION_ACTIVE.fetch_add(1, Ordering::AcqRel) + 1;
        REPLACEMENT_OPERATION_MAX_ACTIVE.fetch_max(active, Ordering::AcqRel);
        if input == 1 {
            std::thread::sleep(Duration::from_millis(40));
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
        REPLACEMENT_OPERATION_ACTIVE.fetch_sub(1, Ordering::AcqRel);
        Ok(input)
    }
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_operation_runs_one_worker_and_only_the_latest_replacement() -> crate::Result<()>
{
    REPLACEMENT_OPERATION_ACTIVE.store(0, Ordering::Release);
    REPLACEMENT_OPERATION_MAX_ACTIVE.store(0, Ordering::Release);
    REPLACEMENT_OPERATION_STARTED.lock().unwrap().clear();

    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("replacement-operation")?,
            Vec::new(),
        ),
    )
    .await?;
    let correlations = Arc::new(Mutex::new(BTreeMap::new()));
    let expired = Arc::new(Mutex::new(BTreeSet::new()));
    let operations = Arc::new(Mutex::new(Vec::new()));
    let exchanges = Arc::new(Mutex::new(Vec::new()));
    let activations = Arc::new(Mutex::new(BTreeMap::new()));
    let mut input = ExecutionInputAdapter::<ReplacementOperationRuntime>::unbound()
        .with_shared_state(
            Arc::clone(&correlations),
            Arc::clone(&expired),
            Arc::clone(&operations),
            Arc::clone(&exchanges),
            Arc::clone(&activations),
        );
    input
        .bind_direct(bus.clone(), "replacement-operation")
        .await?;
    let mut output = ExecutionOutputAdapter::<ReplacementOperationRuntime>::unbound()
        .with_shared_state(correlations, expired, operations, exchanges, activations);
    output.bind_direct(bus.clone(), "replacement-operation");
    let selected = Arc::new(Mutex::new(Some((1, 1))));
    let completions = Arc::new(Mutex::new(Vec::new()));
    let mut runner = RuntimeRunner::new(
        ReplacementOperationRuntime {
            selected: Arc::clone(&selected),
            completions: Arc::clone(&completions),
        },
        ExecutionTime::default(),
        (),
        input,
        output,
    )?;

    runner.poll(ExecutionTime::default())?;
    *selected.lock().unwrap() = Some((2, 2));
    runner.poll(ExecutionTime::from_nanos(1_000_000))?;
    *selected.lock().unwrap() = Some((3, 3));
    runner.poll(ExecutionTime::from_nanos(2_000_000))?;
    runner.poll(ExecutionTime::from_nanos(3_000_000))?;
    assert!(matches!(
        completions.lock().unwrap().as_slice(),
        [(2, Err(OperationInputError::Stale))]
    ));

    tokio::time::sleep(Duration::from_millis(50)).await;
    runner.poll(ExecutionTime::from_nanos(4_000_000))?;
    for index in 5..=20 {
        tokio::time::sleep(Duration::from_millis(2)).await;
        runner.poll(ExecutionTime::from_nanos(index * 1_000_000))?;
        if completions.lock().unwrap().len() == 2 {
            runner.poll(ExecutionTime::from_nanos((index + 1) * 1_000_000))?;
            break;
        }
    }

    assert_eq!(
        REPLACEMENT_OPERATION_STARTED.lock().unwrap().as_slice(),
        &[1, 3],
        "the superseded pending worker must never start"
    );
    assert_eq!(
        REPLACEMENT_OPERATION_MAX_ACTIVE.load(Ordering::Acquire),
        1,
        "replacement cannot overlap the retiring owner"
    );
    let recorded = completions.lock().unwrap().clone();
    assert!(
        matches!(
            recorded.as_slice(),
            [(2, Err(OperationInputError::Stale)), (3, Ok(3))]
        ),
        "unexpected operation completions: {recorded:?}"
    );
    runner.stop()?;
    owner.close().await;
    Ok(())
}

static UNRESPONSIVE_OPERATION_ACTIVE: AtomicBool = AtomicBool::new(false);

struct UnresponsiveOperationRuntime;

impl Runtime for UnresponsiveOperationRuntime {
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
        _inputs: &Self::Inputs,
    ) -> crate::Result<(Self::State, Self::Outputs)> {
        Ok((state, ()))
    }
}

impl RegisteredRuntime for UnresponsiveOperationRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(1, 100, 100);

    fn retain_artifact_metadata() {}
}

#[crate::runtime::outputs]
impl UnresponsiveOperationRuntime {
    #[crate::runtime::outputs::activate(operation)]
    fn activate(&self, _state: &()) -> Option<crate::runtime::Activation<u64, u32>> {
        Some(crate::runtime::Activation::new(1, 1))
    }

    #[crate::runtime::outputs::operation(operation, timeout_ms = 1000, cancel_grace_ms = 5)]
    fn run(input: u32) -> crate::Result<u32> {
        UNRESPONSIVE_OPERATION_ACTIVE.store(true, Ordering::Release);
        std::thread::sleep(Duration::from_millis(50));
        UNRESPONSIVE_OPERATION_ACTIVE.store(false, Ordering::Release);
        Ok(input)
    }
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runner_stop_escalates_when_operation_outlives_its_retirement_grace() -> crate::Result<()> {
    UNRESPONSIVE_OPERATION_ACTIVE.store(false, Ordering::Release);
    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("unresponsive-operation")?,
            Vec::new(),
        ),
    )
    .await?;
    let correlations = Arc::new(Mutex::new(BTreeMap::new()));
    let expired = Arc::new(Mutex::new(BTreeSet::new()));
    let operations = Arc::new(Mutex::new(Vec::new()));
    let exchanges = Arc::new(Mutex::new(Vec::new()));
    let activations = Arc::new(Mutex::new(BTreeMap::new()));
    let mut input = ExecutionInputAdapter::<UnresponsiveOperationRuntime>::unbound()
        .with_shared_state(
            Arc::clone(&correlations),
            Arc::clone(&expired),
            Arc::clone(&operations),
            Arc::clone(&exchanges),
            Arc::clone(&activations),
        );
    input
        .bind_direct(bus.clone(), "unresponsive-operation")
        .await?;
    let mut output = ExecutionOutputAdapter::<UnresponsiveOperationRuntime>::unbound()
        .with_shared_state(correlations, expired, operations, exchanges, activations);
    output.bind_direct(bus, "unresponsive-operation");
    let mut runner = RuntimeRunner::new(
        UnresponsiveOperationRuntime,
        ExecutionTime::default(),
        (),
        input,
        output,
    )?;
    runner.poll(ExecutionTime::default())?;
    tokio::time::timeout(Duration::from_secs(1), async {
        while !UNRESPONSIVE_OPERATION_ACTIVE.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    let error = runner
        .stop()
        .expect_err("an unresponsive operation requires process replacement");
    assert!(error.chain().any(|cause| {
        cause.downcast_ref::<RunnerError>().is_some_and(|error| {
            matches!(
                error,
                RunnerError::ProcessTerminationRequired {
                    field: "operation",
                    ..
                }
            )
        })
    }));
    assert!(
        UNRESPONSIVE_OPERATION_ACTIVE.load(Ordering::Acquire),
        "stop must not join an owner after its retirement grace"
    );
    owner.close().await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    Ok(())
}

#[crate::message(package = "phoxal.runtime.test")]
struct ReadRequest {
    #[phoxal(tag = 1)]
    value: u32,
}

#[crate::message(package = "phoxal.runtime.test")]
struct ReadResponse {
    #[phoxal(tag = 1)]
    value: u32,
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

struct PublicReadState(u32);

impl Runtime for PublicReadRuntime {
    type Config = ();
    type State = PublicReadState;
    type Inputs = ReadClientInputs;
    type Outputs = ();

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
        Ok(PublicReadState(41))
    }

    fn step(
        &self,
        _ctx: &StepContext,
        mut state: Self::State,
        _inputs: &Self::Inputs,
    ) -> crate::Result<(Self::State, Self::Outputs)> {
        state.0 = state.0.saturating_add(1);
        Ok((state, ()))
    }
}

impl RegisteredRuntime for PublicReadRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(10, 1000, 100);

    fn retain_artifact_metadata() {}
}

#[crate::runtime::outputs]
impl PublicReadRuntime {
    fn view(&self, state: &PublicReadState) -> u32 {
        state.0
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
    observations: Arc<Mutex<Vec<ReadObservation>>>,
}

type ReadObservation = (crate::runtime::ReadStatus, bool, Option<(u32, String, u64)>);

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
        let retained = inputs.read.retained_success().map(|success| {
            (
                success.response().value,
                success.provenance().source().to_owned(),
                success.provenance().capture_time().as_nanos(),
            )
        });
        self.observations.lock().unwrap().push((
            inputs.read.status(),
            inputs.read.new_completion().is_some(),
            retained,
        ));
        if let Some(completion) = inputs.read.new_completion()
            && let Ok(response) = completion.result()
        {
            *self.response.lock().expect("read response lock") = Some(response.value);
        }
        Ok((state, ()))
    }
}

impl RegisteredRuntime for ReadClientRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(1, 100, 100);

    fn retain_artifact_metadata() {}
}

#[crate::runtime::outputs]
impl ReadClientRuntime {
    #[crate::runtime::outputs::activate(read, timeout_ms = 100, refresh_every_steps = 4)]
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
    observations: Arc<Mutex<Vec<ReadObservation>>>,
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
    let activations = Arc::new(Mutex::new(BTreeMap::new()));
    let mut input = ExecutionInputAdapter::<ReadClientRuntime>::unbound().with_shared_state(
        Arc::clone(&correlations),
        Arc::clone(&expired),
        Arc::clone(&operations),
        Arc::clone(&exchanges),
        Arc::clone(&activations),
    );
    input.bind(bus.clone(), manifest).await?;
    let mut output = ExecutionOutputAdapter::<ReadClientRuntime>::unbound().with_shared_state(
        correlations,
        expired,
        operations,
        exchanges,
        activations,
    );
    output
        .bind(bus.clone(), &manifest.instance_id, manifest)
        .await?;

    RuntimeRunner::new(
        ReadClientRuntime {
            selected_key,
            response,
            observations,
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
    let signature = SourceMethodSignature {
        endpoint: READ_PORT.signature().name.to_owned(),
        service: READ_PORT.signature().service.to_owned(),
        method: READ_PORT.signature().method.to_owned(),
        shape: crate::artifact::MethodShape::Call,
        request: READ_PORT.signature().request.to_owned(),
        response: READ_PORT.signature().response.to_owned(),
        retained_latest: false,
        lease_valid_for_ms: None,
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
                role: "method".to_owned(),
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
        requirement_destinations: BTreeMap::new(),
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
    let observations = Arc::new(Mutex::new(Vec::new()));
    let selected_key = Arc::new(Mutex::new(Some(9)));
    let mut runner = read_client_runner(
        &bus,
        &manifest,
        Arc::clone(&selected_key),
        Arc::clone(&response),
        Arc::clone(&observations),
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
    let other_observations = Arc::new(Mutex::new(Vec::new()));
    let mut other = read_client_runner(
        &bus,
        &other_manifest,
        Arc::clone(&other_key),
        Arc::clone(&other_response),
        other_observations,
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
    assert_eq!(
        observations.lock().unwrap().as_slice(),
        &[
            (crate::runtime::ReadStatus::Inactive, false, None),
            (crate::runtime::ReadStatus::Pending, false, None),
            (crate::runtime::ReadStatus::Completed, true, None),
            (
                crate::runtime::ReadStatus::Completed,
                false,
                Some((42, "reader".to_owned(), 2_000_000)),
            ),
        ]
    );
    runner.poll(ExecutionTime::from_nanos(4_000_000))?;
    let refresh = tokio::time::timeout(Duration::from_secs(2), requests.recv_async())
        .await?
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let refresh = WireSample::from_zenoh(refresh)?;
    runner.poll(ExecutionTime::from_nanos(5_000_000))?;
    let refresh_metadata = refresh.metadata();
    let failure_metadata = crate::runtime::transport::reply_metadata(
        "reader",
        StepContext::first(
            ExecutionTime::from_nanos(6_000_000),
            ExecutionDuration::from_millis(1),
        ),
        refresh_metadata.command_id.expect("refresh command id"),
        refresh_metadata
            .eligible_boundary
            .expect("refresh boundary"),
        refresh_metadata.caller_rank.expect("refresh caller rank"),
    )
    .with_caller(
        refresh_metadata
            .caller
            .clone()
            .expect("refresh request caller"),
    );
    crate::runtime::transport::PreparedOutput::read_refusal(
        READ_PORT.signature(),
        crate::runtime::transport::WireControl::Failed,
        failure_metadata,
    )
    .publish_async(&bus, "reader")
    .await?;
    tokio::time::sleep(Duration::from_millis(10)).await;
    runner.poll(ExecutionTime::from_nanos(6_000_000))?;
    runner.poll(ExecutionTime::from_nanos(7_000_000))?;
    let refresh_observations = observations.lock().unwrap().clone();
    assert_eq!(
        &refresh_observations[4..8],
        &[
            (
                crate::runtime::ReadStatus::Completed,
                false,
                Some((42, "reader".to_owned(), 2_000_000)),
            ),
            (
                crate::runtime::ReadStatus::Pending,
                false,
                Some((42, "reader".to_owned(), 2_000_000)),
            ),
            (
                crate::runtime::ReadStatus::Completed,
                true,
                Some((42, "reader".to_owned(), 2_000_000)),
            ),
            (
                crate::runtime::ReadStatus::Completed,
                false,
                Some((42, "reader".to_owned(), 2_000_000)),
            ),
        ]
    );
    *selected_key.lock().unwrap() = None;
    runner.poll(ExecutionTime::from_nanos(8_000_000))?;
    *selected_key.lock().unwrap() = Some(9);
    runner.poll(ExecutionTime::from_nanos(9_000_000))?;
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
    runner.poll(ExecutionTime::from_nanos(10_000_000))?;
    let replacement = tokio::time::timeout(Duration::from_secs(2), requests.recv_async())
        .await?
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let replacement = WireSample::from_zenoh(replacement)?;
    assert_ne!(
        replacement.metadata().command_id,
        retry.metadata().command_id
    );
    runner.poll(ExecutionTime::from_nanos(11_000_000))?;
    assert!(
        requests
            .try_recv()
            .expect("request queue readable")
            .is_none(),
        "replacement key also starts once"
    );
    let replacement_observations = observations.lock().unwrap().clone();
    assert_eq!(
        replacement_observations[9],
        (crate::runtime::ReadStatus::Inactive, false, None),
        "retiring a key clears its retained success"
    );
    assert_eq!(
        replacement_observations[11],
        (crate::runtime::ReadStatus::Pending, false, None),
        "a replacement key cannot inherit historical success"
    );
    runner.reset(ExecutionTime::from_nanos(12_000_000), ())?;
    runner.poll(ExecutionTime::from_nanos(12_000_000))?;
    assert_eq!(
        observations.lock().unwrap().last(),
        Some(&(crate::runtime::ReadStatus::Inactive, false, None)),
        "reset starts with no completion or retained success"
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
        requirement_destinations: BTreeMap::new(),
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

#[crate::message(package = "phoxal.runtime.test")]
struct TypedState {
    #[phoxal(tag = 1)]
    value: i32,
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

    fn retain_artifact_metadata() {}
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
    let signature = SourceMethodSignature {
        endpoint: SOURCE_STATE.name.to_owned(),
        service: SOURCE_STATE.service.to_owned(),
        method: SOURCE_STATE.method.to_owned(),
        shape: crate::artifact::MethodShape::Observation,
        request: SOURCE_STATE.request.to_owned(),
        response: SOURCE_STATE.response.to_owned(),
        retained_latest: true,
        lease_valid_for_ms: None,
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
                role: "method".to_owned(),
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
        requirement_destinations: BTreeMap::new(),
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

/// The conversion-role scheduling shape: an arrival-aligned runtime whose
/// step mirrors the generated conversion glue — a bounded-freshness Latest
/// input that forwards only fresh samples, and an optional conversion
/// failure surfaced as a step error.
struct ArrivalRuntime {
    seen: Arc<Mutex<Option<i32>>>,
    fail_on_admit: bool,
}

impl Runtime for ArrivalRuntime {
    type Config = ();
    type State = ();
    type Inputs = ArrivalInputs;
    type Outputs = ();

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> crate::Result<Self::State> {
        Ok(())
    }

    fn step(
        &self,
        ctx: &StepContext,
        state: Self::State,
        inputs: &Self::Inputs,
    ) -> crate::Result<(Self::State, Self::Outputs)> {
        if inputs.state.is_fresh_at(ctx.now(), Some(50))
            && let Some(sample) = inputs.state.sample()
        {
            if self.fail_on_admit {
                return Err(anyhow::anyhow!("converted payload rejected"));
            }
            *self.seen.lock().expect("arrival observation lock") = Some(sample.payload().value);
        }
        Ok((state, ()))
    }
}

impl RegisteredRuntime for ArrivalRuntime {
    const SPEC: RuntimeSpec = RuntimeSpec::from_millis(20, 200, 200).with_arrival_releases();

    fn retain_artifact_metadata() {}
}

impl crate::runtime::outputs::OutputBindings for ArrivalRuntime {
    const FIELDS: &'static [crate::runtime::outputs::OutputField] = &[];
}

#[crate::runtime::inputs]
struct ArrivalInputs {
    #[crate::runtime::input(max_age_ms = 50)]
    state: crate::runtime::Latest<TypedState>,
}

async fn arrival_runner(
    seen: Arc<Mutex<Option<i32>>>,
    fail_on_admit: bool,
) -> crate::Result<(
    RuntimeRunner<
        ArrivalRuntime,
        super::ExecutionInputAdapter<ArrivalRuntime>,
        super::ExecutionOutputAdapter<ArrivalRuntime>,
    >,
    crate::runtime::connection::ConnectionOwner,
    crate::runtime::connection::Connection,
)> {
    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("arrival-consumer")?,
            Vec::new(),
        ),
    )
    .await?;
    let signature = SourceMethodSignature {
        endpoint: SOURCE_STATE.name.to_owned(),
        service: SOURCE_STATE.service.to_owned(),
        method: SOURCE_STATE.method.to_owned(),
        shape: crate::artifact::MethodShape::Observation,
        request: SOURCE_STATE.request.to_owned(),
        response: SOURCE_STATE.response.to_owned(),
        retained_latest: true,
        lease_valid_for_ms: None,
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
            period_ms: Some(20),
            timeout_ms: Some(200),
            init_timeout_ms: Some(200),
            inputs: Vec::new(),
            transient_outputs: Vec::new(),
            service_outputs: vec![SourceOutputRecord {
                name: "state".to_owned(),
                role: "method".to_owned(),
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
        robot_id: "arrival-test".to_owned(),
        instance_id: "consumer".to_owned(),
        executable: PathBuf::from("arrival-test"),
        executable_sha256: "00".repeat(32),
        config: Value::Object(serde_json::Map::new()),
        connections,
        requirement_destinations: BTreeMap::new(),
        artifacts,
        scenario_producers: BTreeMap::new(),
        observation_providers: BTreeMap::new(),
    };
    let mut input = super::ExecutionInputAdapter::<ArrivalRuntime>::unbound();
    input.bind(bus.clone(), &manifest).await?;
    let mut output = super::ExecutionOutputAdapter::<ArrivalRuntime>::unbound();
    output.bind_direct(bus.clone(), "consumer");
    let runner = RuntimeRunner::new(
        ArrivalRuntime {
            seen,
            fail_on_admit,
        },
        ExecutionTime::default(),
        (),
        input,
        output,
    )?;
    Ok((runner, owner, bus))
}

async fn publish_state(
    bus: &crate::runtime::connection::Connection,
    stamp: ExecutionTime,
) -> crate::Result<()> {
    let prepared = PreparedOutput::response(
        SOURCE_STATE,
        &TypedState { value: 42 },
        64,
        crate::runtime::transport::publication_metadata(
            "producer",
            StepContext::first(stamp, ExecutionDuration::from_millis(20)),
            0,
        ),
    )?;
    crate::runtime::transport::publish_batch(bus, "producer", &[prepared])?;
    tokio::time::sleep(Duration::from_millis(30)).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_arrival_pulled_release_converts_between_period_ticks() -> crate::Result<()> {
    const MS: u64 = 1_000_000;
    let seen = Arc::new(Mutex::new(None));
    let (mut runner, owner, bus) = arrival_runner(seen.clone(), false).await?;
    assert!(matches!(
        runner.poll(ExecutionTime::from_nanos(0)),
        Ok(PollOutcome::Accepted {
            invocation_index: 0
        })
    ));
    publish_state(&bus, ExecutionTime::from_nanos(5 * MS)).await?;
    assert_eq!(
        *seen.lock().expect("arrival observation lock"),
        None,
        "no release ran between ticks"
    );
    // The closeout review's probe: mid-period the nominal schedule is not
    // due, so a plain poll waits for the tick...
    assert!(matches!(
        runner.poll(ExecutionTime::from_nanos(5 * MS)),
        Ok(PollOutcome::NotDue { .. })
    ));
    // ...while the admitted arrival pulls the release to its own instant
    // and the fresh sample converts immediately.
    runner.arrive(ExecutionTime::from_nanos(5 * MS));
    assert!(matches!(
        runner.poll(ExecutionTime::from_nanos(5 * MS)),
        Ok(PollOutcome::Accepted {
            invocation_index: 1
        })
    ));
    assert_eq!(
        *seen.lock().expect("arrival observation lock"),
        Some(42),
        "the arrival-triggered release forwarded the fresh sample"
    );
    runner.stop()?;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_sample_through_an_arrival_release_is_not_forwarded() -> crate::Result<()> {
    const MS: u64 = 1_000_000;
    let seen = Arc::new(Mutex::new(None));
    let (mut runner, owner, bus) = arrival_runner(seen.clone(), false).await?;
    // A late first poll (60 ms) skips missed releases; the schedule's next
    // release is 80 ms, so the 70 ms polls below only run through arrivals.
    assert!(matches!(
        runner.poll(ExecutionTime::from_nanos(60 * MS)),
        Ok(PollOutcome::Accepted {
            invocation_index: 0
        })
    ));
    // A sample stamped at the start is already older than the 50 ms bound.
    publish_state(&bus, ExecutionTime::from_nanos(0)).await?;
    runner.arrive(ExecutionTime::from_nanos(70 * MS));
    assert!(matches!(
        runner.poll(ExecutionTime::from_nanos(70 * MS)),
        Ok(PollOutcome::Accepted {
            invocation_index: 1
        })
    ));
    assert_eq!(
        *seen.lock().expect("arrival observation lock"),
        None,
        "stale data must not be forwarded through the arrival path"
    );
    // A fresh sample through the same path converts.
    publish_state(&bus, ExecutionTime::from_nanos(65 * MS)).await?;
    runner.arrive(ExecutionTime::from_nanos(75 * MS));
    assert!(runner.poll(ExecutionTime::from_nanos(75 * MS)).is_ok());
    assert_eq!(*seen.lock().expect("arrival observation lock"), Some(42));
    runner.stop()?;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_conversion_failure_through_an_arrival_release_fails_visibly() -> crate::Result<()> {
    const MS: u64 = 1_000_000;
    let seen = Arc::new(Mutex::new(None));
    let (mut runner, owner, bus) = arrival_runner(seen.clone(), true).await?;
    assert!(runner.poll(ExecutionTime::from_nanos(0)).is_ok());
    publish_state(&bus, ExecutionTime::from_nanos(5 * MS)).await?;
    runner.arrive(ExecutionTime::from_nanos(5 * MS));
    let error = runner
        .poll(ExecutionTime::from_nanos(5 * MS))
        .expect_err("a conversion failure must surface, never publish a default");
    assert!(
        error.to_string().contains("converted payload rejected"),
        "{error}"
    );
    let _ = owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_holds_through_arrival_releases() -> crate::Result<()> {
    const MS: u64 = 1_000_000;
    let seen = Arc::new(Mutex::new(None));
    let (mut runner, owner, bus) = arrival_runner(seen.clone(), false).await?;
    assert!(runner.poll(ExecutionTime::from_nanos(0)).is_ok());
    publish_state(&bus, ExecutionTime::from_nanos(5 * MS)).await?;
    runner.stop()?;
    runner.arrive(ExecutionTime::from_nanos(5 * MS));
    assert!(matches!(
        runner.poll(ExecutionTime::from_nanos(5 * MS)),
        Ok(PollOutcome::Stopped)
    ));
    assert_eq!(*seen.lock().expect("arrival observation lock"), None);
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
            expected_sources: BTreeSet::from(["producer".to_owned()]),
            expected_callers: BTreeSet::new(),
            target: format!("consumer.{field}"),
            port: SOURCE_STATE.name.to_owned(),
            direction: "publish".to_owned(),
            ack_leg: "delivery-ack".to_owned(),
            cancel: cancel.clone(),
            reply_admission: None,
            arrivals: None,
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
        requirement_destinations: BTreeMap::new(),
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

    runner.outputs.pin_read_views("timeline", 3)?;
    let mut metadata = transport::request_metadata(
        "caller",
        "caller.read",
        StepContext::first(
            ExecutionTime::from_nanos(10_000_000),
            ExecutionDuration::from_millis(10),
        ),
        3,
        4,
        0,
    );
    metadata.request_timeout_ms = Some(50);
    let mut request = PreparedOutput::request(
        READ_PORT.signature(),
        &ReadRequest { value: 1 },
        64,
        metadata,
    )?
    .for_instance("reader");
    request.stamp_delivery_identity(&execution.to_string(), "timeline", 3, 0)?;
    request.publish_async(&bus, "caller").await?;
    tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
        .await?
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let rejected: execution_wire::DeliveryAck =
        tokio::time::timeout(Duration::from_secs(2), recv_execution(&request_acks)).await??;
    assert!(!rejected.admitted);
    assert_eq!(rejected.boundary, 3);
    assert!(
        rejected
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("timed out")),
        "reply acknowledgement loss must reject the required request with its deadline"
    );

    runner.outputs.pin_read_views("timeline", 4)?;
    let mut metadata = transport::request_metadata(
        "caller",
        "caller.read",
        StepContext::first(
            ExecutionTime::from_nanos(10_000_000),
            ExecutionDuration::from_millis(10),
        ),
        4,
        5,
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
    request.stamp_delivery_identity(&execution.to_string(), "timeline", 4, 0)?;
    request.publish_async(&bus, "caller").await?;
    let reply = tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
        .await?
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let wire = WireSample::from_zenoh(reply)?;

    runner.reset(ExecutionTime::from_nanos(20_000_000), ())?;
    publish_execution(
        &bus,
        "reader",
        "read-reply-ack/read",
        &execution_wire::DeliveryAck {
            execution_id: execution.to_string(),
            timeline_id: "timeline".to_owned(),
            boundary: 4,
            source: "reader".to_owned(),
            target: "caller.read".to_owned(),
            port: "read".to_owned(),
            direction: "reply".to_owned(),
            sequence: 5,
            item: 0,
            bytes: wire.payload().len() as u64,
            admitted: true,
            detail: None,
        },
    )
    .await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), request_acks.recv_async())
            .await
            .is_err(),
        "a reset generation cannot acknowledge a reply after revocation"
    );
    runner.stop()?;
    owner.close().await;
    Ok(())
}

async fn send_external_read(
    bus: &crate::runtime::connection::Connection,
    id: u64,
    value: u32,
) -> crate::Result<()> {
    send_external_read_bytes(bus, id, ReadRequest { value }.encode_to_vec()).await
}

async fn send_external_read_bytes(
    bus: &crate::runtime::connection::Connection,
    id: u64,
    payload: Vec<u8>,
) -> crate::Result<()> {
    let metadata = RuntimeWireMetadata::external_request(ExecutionTime::default(), id, 0, id);
    bus.session()?
        .put(
            bus.full_key(&transport::port_key("reader", "read", "request")),
            payload,
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
        requirement_destinations: BTreeMap::new(),
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

    send_external_read_bytes(&bus, 5, vec![0; 65]).await?;
    let oversized_request = WireSample::from_zenoh(
        tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
            .await?
            .map_err(|e| anyhow::anyhow!(e.to_string()))?,
    )?;
    assert_eq!(
        oversized_request.metadata().wire_control()?,
        transport::WireControl::Oversized
    );
    assert!(oversized_request.payload().is_empty());
    assert_eq!(
        calls.load(Ordering::Acquire),
        3,
        "an oversized request cannot enter the handler"
    );

    send_external_read_bytes(&bus, 6, vec![0x80]).await?;
    let malformed = WireSample::from_zenoh(
        tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
            .await?
            .map_err(|e| anyhow::anyhow!(e.to_string()))?,
    )?;
    assert_eq!(
        malformed.metadata().wire_control()?,
        transport::WireControl::Failed
    );
    assert!(malformed.payload().is_empty());
    assert_eq!(
        calls.load(Ordering::Acquire),
        3,
        "a malformed request cannot enter the typed handler"
    );

    send_external_read(&bus, 7, 1).await?;
    wait_entered(4).await?;
    runner.reset(ExecutionTime::default(), ())?;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), replies.recv_async())
            .await
            .is_err(),
        "retired view cannot publish after reset"
    );
    send_external_read(&bus, 8, 1).await?;
    let reset = WireSample::from_zenoh(
        tokio::time::timeout(Duration::from_secs(2), replies.recv_async())
            .await?
            .map_err(|e| anyhow::anyhow!(e.to_string()))?,
    )?;
    assert_eq!(ReadResponse::decode(reset.payload())?.value, 42);
    send_external_read(&bus, 9, 1).await?;
    wait_entered(6).await?;
    runner.stop()?;
    owner.close().await;
    assert!(
        !matches!(replies.try_recv(), Ok(Some(_))),
        "stop joins the worker without publishing its retired result"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Authored-runtime lifecycle over the real transport adapters: bootstrap
// publication before any step, no setpoint authority at initialization, and
// reset republication under a fresh fence. The fixture is authored through
// the inherent macro, like every new authoring-model runtime.
// ---------------------------------------------------------------------------

#[phoxal::messages(package = "phoxal.tests.runner.lifecycle.v1")]
mod lifecycle {
    use phoxal::contracts::{Latest, Queue, State};

    pub struct TickEvent {
        #[phoxal(tag = 1)]
        pub sequence: u64,
    }

    pub struct MeterState {
        #[phoxal(tag = 1)]
        pub steps: u64,
        #[phoxal(tag = 2)]
        pub consumed: u64,
    }

    pub struct FlowSetpoint {
        #[phoxal(tag = 1)]
        pub rate: u64,
    }

    /// The meter's endpoint contract.
    #[phoxal::endpoints]
    pub struct MeterApi {
        #[phoxal::input(max_items = 4, max_bytes = 256)]
        ticks: Queue<TickEvent>,

        #[phoxal::output]
        status: State<MeterState>,

        #[phoxal::output(projection = state, lease_ms = 100, max_bytes = 256)]
        target: Latest<FlowSetpoint>,
    }
}

use lifecycle::{FlowSetpoint, MeterState, TickEvent};

/// One authored meter: every accepted invocation advances `steps`, every
/// admitted tick advances `consumed`, and the leased projection always
/// carries a real value, so initialization and ordinary invocations produce
/// distinguishable publications.
pub(crate) struct Meter {
    steps: u64,
    consumed: u64,
}

#[phoxal::runtime(contract = lifecycle::MeterApi, period_ms = 10)]
impl Meter {
    #[init]
    fn init(_config: ()) -> phoxal::Result<Self> {
        Ok(Self {
            steps: 0,
            consumed: 0,
        })
    }

    #[handle(ticks)]
    fn on_tick(
        &mut self,
        _ctx: &mut crate::runtime::Context<'_, Self>,
        _tick: TickEvent,
    ) -> phoxal::Result<()> {
        self.consumed = self.consumed.saturating_add(1);
        Ok(())
    }

    #[step]
    fn advance(&mut self, _ctx: &mut crate::runtime::Context<'_, Self>) -> phoxal::Result<()> {
        self.steps = self.steps.saturating_add(1);
        Ok(())
    }

    #[publish(status)]
    fn status(&self) -> MeterState {
        MeterState {
            steps: self.steps,
            consumed: self.consumed,
        }
    }

    #[publish(target)]
    fn target(&self) -> Option<FlowSetpoint> {
        Some(FlowSetpoint { rate: 7 })
    }
}

const LIFECYCLE_TICKS: crate::port::PortSignature = crate::port::PortSignature::with_descriptor(
    "ticks",
    "phoxal.tests.runner.lifecycle.v1",
    "Ticks",
    crate::port::PortKind::Event,
    "google.protobuf.Empty",
    "phoxal.tests.runner.lifecycle.v1.TickEvent",
    &[],
);

/// Binds the meter's input and output adapters over one bus whose manifest
/// routes the queued tick input from a connected producer. The test
/// subscribes on the same bus before constructing the runner, so bootstrap
/// publications are observed from the very first record.
async fn lifecycle_adapters() -> crate::Result<(
    ExecutionInputAdapter<phoxal_runtime_meter::Adapter>,
    ExecutionOutputAdapter<phoxal_runtime_meter::Adapter>,
    crate::runtime::connection::ConnectionOwner,
    crate::runtime::connection::Connection,
)> {
    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("lifecycle-meter")?,
            Vec::new(),
        ),
    )
    .await?;
    let signature = SourceMethodSignature {
        endpoint: LIFECYCLE_TICKS.name.to_owned(),
        service: LIFECYCLE_TICKS.service.to_owned(),
        method: LIFECYCLE_TICKS.method.to_owned(),
        shape: crate::artifact::MethodShape::Observation,
        request: LIFECYCLE_TICKS.request.to_owned(),
        response: LIFECYCLE_TICKS.response.to_owned(),
        retained_latest: false,
        lease_valid_for_ms: None,
    };
    let mut connections = BTreeMap::new();
    connections.insert("meter.ticks".to_owned(), vec!["producer.ticks".to_owned()]);
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        "producer".to_owned(),
        SourceRuntimeRecord {
            period_ms: Some(10),
            timeout_ms: Some(100),
            init_timeout_ms: Some(100),
            inputs: Vec::new(),
            transient_outputs: vec![SourceOutputRecord {
                name: "ticks".to_owned(),
                role: "method".to_owned(),
                port: Some("ticks".to_owned()),
                signature: Some(signature),
                input: None,
                max_items: Some(4),
                max_bytes: Some(256),
                max_request_bytes: None,
            }],
            service_outputs: Vec::new(),
        },
    );
    let manifest = RuntimeLaunchManifest {
        root: PathBuf::from("."),
        robot_id: "lifecycle-test".to_owned(),
        instance_id: "meter".to_owned(),
        executable: PathBuf::from("lifecycle-test"),
        executable_sha256: "00".repeat(32),
        config: Value::Object(serde_json::Map::new()),
        connections,
        requirement_destinations: BTreeMap::new(),
        artifacts,
        scenario_producers: BTreeMap::new(),
        observation_providers: BTreeMap::new(),
    };
    let mut input = ExecutionInputAdapter::<phoxal_runtime_meter::Adapter>::unbound();
    input.bind(bus.clone(), &manifest).await?;
    let mut output = ExecutionOutputAdapter::<phoxal_runtime_meter::Adapter>::unbound();
    output.bind_direct(bus.clone(), "meter");
    Ok((input, output, owner, bus))
}

/// Receives and decodes the next retained status publication.
async fn next_meter_status(
    statuses: &mut zenoh::pubsub::Subscriber<
        zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>,
    >,
) -> crate::Result<MeterState> {
    use prost::Message as _;

    let sample = tokio::time::timeout(Duration::from_secs(2), statuses.recv_async())
        .await?
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let wire = crate::runtime::transport::WireSample::from_zenoh(sample)?;
    MeterState::decode(wire.payload()).map_err(|error| anyhow::anyhow!(error.to_string()))
}

/// Publishes one tick event from the connected producer.
fn publish_tick(bus: &crate::runtime::connection::Connection, sequence: u64) -> crate::Result<()> {
    let prepared = PreparedOutput::response(
        LIFECYCLE_TICKS,
        &TickEvent { sequence },
        256,
        crate::runtime::transport::publication_metadata(
            "producer",
            StepContext::first(ExecutionTime::default(), ExecutionDuration::from_millis(10)),
            sequence,
        ),
    )?;
    crate::runtime::transport::publish_batch(bus, "producer", &[prepared])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authored_bootstrap_publishes_initial_state_and_no_setpoint_before_any_step()
-> crate::Result<()> {
    use prost::Message as _;
    use zenoh::handlers::FifoChannel;
    use zenoh::key_expr::OwnedKeyExpr;

    let (input, output, owner, bus) = lifecycle_adapters().await?;
    let session = bus.session()?;
    let status_key = bus.full_key(&crate::runtime::transport::port_key(
        "meter", "status", "publish",
    ));
    let mut statuses = session
        .declare_subscriber(OwnedKeyExpr::new(status_key).expect("status key"))
        .with(FifoChannel::new(16))
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let target_key = bus.full_key(&crate::runtime::transport::port_key(
        "meter", "target", "publish",
    ));
    let targets = session
        .declare_subscriber(OwnedKeyExpr::new(target_key).expect("target key"))
        .with(FifoChannel::new(16))
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;

    // `RuntimeRunner::new` runs initialization and its bootstrap phase; no
    // invocation has been polled yet. The initial retained status must
    // arrive before any step ran — it carries the initializer's values,
    // which no ordinary invocation produces (the first step advances
    // `steps` past zero).
    let mut runner = RuntimeRunner::new(
        phoxal_runtime_meter::Adapter::new(),
        ExecutionTime::default(),
        (),
        input,
        output,
    )?;
    let initial = next_meter_status(&mut statuses).await?;
    assert_eq!(
        (initial.steps, initial.consumed),
        (0, 0),
        "bootstrap publishes the initializer's state before any step"
    );

    // The leased projection returns a real value from the initialized
    // state, yet bootstrap must not publish it: no setpoint value (and no
    // lease renewal) may appear on the target port before the first
    // invocation.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), targets.recv_async())
            .await
            .is_err(),
        "bootstrap publishes no setpoint on the leased target port"
    );

    // The projection itself is live: the first accepted invocation
    // publishes both the advanced status and the real setpoint value.
    assert!(matches!(
        runner.poll(ExecutionTime::from_nanos(10_000_000)),
        Ok(PollOutcome::Accepted {
            invocation_index: 0
        })
    ));
    let advanced = next_meter_status(&mut statuses).await?;
    assert_eq!((advanced.steps, advanced.consumed), (1, 0));
    let sample = tokio::time::timeout(Duration::from_secs(2), targets.recv_async())
        .await?
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let wire = crate::runtime::transport::WireSample::from_zenoh(sample)?;
    assert_eq!(FlowSetpoint::decode(wire.payload())?.rate, 7);

    runner.stop()?;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authored_reset_republishes_initial_state_before_the_next_step() -> crate::Result<()> {
    use zenoh::handlers::FifoChannel;
    use zenoh::key_expr::OwnedKeyExpr;

    let (input, output, owner, bus) = lifecycle_adapters().await?;
    let session = bus.session()?;
    let status_key = bus.full_key(&crate::runtime::transport::port_key(
        "meter", "status", "publish",
    ));
    let mut statuses = session
        .declare_subscriber(OwnedKeyExpr::new(status_key).expect("status key"))
        .with(FifoChannel::new(16))
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;

    let mut runner = RuntimeRunner::new(
        phoxal_runtime_meter::Adapter::new(),
        ExecutionTime::default(),
        (),
        input,
        output,
    )?;
    let _initial = next_meter_status(&mut statuses).await?;

    // One admitted tick reaches the handler through the connected graph
    // edge; the resulting status records it. Admission into the receiver's
    // bounded queue is awaited deterministically on the queue itself
    // rather than a fixed sleep.
    publish_tick(&bus, 1)?;
    wait_for_queued_ticks(&runner, 1).await?;
    assert!(matches!(
        runner.poll(ExecutionTime::from_nanos(10_000_000)),
        Ok(PollOutcome::Accepted {
            invocation_index: 0
        })
    ));
    let consumed = next_meter_status(&mut statuses).await?;
    assert_eq!((consumed.steps, consumed.consumed), (1, 1));

    // A second tick is admitted but left UNCONSUMED: no invocation runs
    // before the reset, so the item sits in the receiver's bounded queue.
    publish_tick(&bus, 2)?;
    wait_for_queued_ticks(&runner, 1).await?;

    // Reset reconstructs the initialized runner state, clears the input
    // adapters, restarts the invocation index, and republishes the initial
    // state BEFORE the next invocation: the reconstructed publication again
    // carries the initializer's values, which no post-step state can
    // produce. This drives runner state/input reset on one bus execution
    // identity; it does not verify a new transport execution or simulation
    // timeline fence, which process-tier simulation reset owns.
    runner.reset(ExecutionTime::from_nanos(20_000_000), ())?;
    assert_eq!(
        queued_ticks(&runner),
        0,
        "reset clears the admitted-but-unconsumed tick from the receiver queue"
    );
    let reconstructed = next_meter_status(&mut statuses).await?;
    assert_eq!(
        (reconstructed.steps, reconstructed.consumed),
        (0, 0),
        "reset republishes the reconstructed initial state before the next step"
    );

    // The admitted-but-unconsumed tick does not survive reset: the next
    // invocation advances only the step counter.
    assert!(matches!(
        runner.poll(ExecutionTime::from_nanos(30_000_000)),
        Ok(PollOutcome::Accepted {
            invocation_index: 0
        })
    ));
    let after = next_meter_status(&mut statuses).await?;
    assert_eq!(
        (after.steps, after.consumed),
        (1, 0),
        "the admitted-but-unconsumed tick does not resume after reset"
    );

    // A newly admitted tick still reaches the handler after the reset.
    publish_tick(&bus, 3)?;
    wait_for_queued_ticks(&runner, 1).await?;
    assert!(matches!(
        runner.poll(ExecutionTime::from_nanos(40_000_000)),
        Ok(PollOutcome::Accepted {
            invocation_index: 1
        })
    ));
    let resumed = next_meter_status(&mut statuses).await?;
    assert_eq!(
        (resumed.steps, resumed.consumed),
        (2, 1),
        "a newly admitted tick still reaches the handler after reset"
    );

    runner.stop()?;
    owner.close().await;
    Ok(())
}

/// The number of ticks currently admitted in the meter's receiver queue.
fn queued_ticks(
    runner: &RuntimeRunner<
        phoxal_runtime_meter::Adapter,
        ExecutionInputAdapter<phoxal_runtime_meter::Adapter>,
        ExecutionOutputAdapter<phoxal_runtime_meter::Adapter>,
    >,
) -> usize {
    runner
        .inputs
        .subscriptions
        .iter()
        .find(|subscription| subscription.field == "ticks")
        .and_then(|subscription| subscription.delivery.as_ref())
        .map(|delivery| delivery.queue.lock().expect("tick queue lock").items.len())
        .unwrap_or(0)
}

/// Waits, bounded, until the meter's receiver queue holds the expected
/// number of admitted ticks.
async fn wait_for_queued_ticks(
    runner: &RuntimeRunner<
        phoxal_runtime_meter::Adapter,
        ExecutionInputAdapter<phoxal_runtime_meter::Adapter>,
        ExecutionOutputAdapter<phoxal_runtime_meter::Adapter>,
    >,
    expected: usize,
) -> crate::Result<()> {
    tokio::time::timeout(Duration::from_secs(2), async {
        while queued_ticks(runner) < expected {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("tick admission timed out"))
}

// ---------------------------------------------------------------------------
// Reverse graph delivery: a brain's state export feeding a provider's
// observation input through a real graph connection.
// ---------------------------------------------------------------------------

#[phoxal::messages(package = "phoxal.runtime.reverse.v1")]
mod reverse {
    use phoxal::contracts::Latest;

    pub struct Report {
        #[phoxal(tag = 1)]
        pub value: u64,
    }

    /// The brain-side contract: only the export.
    #[phoxal::endpoints]
    pub struct ExporterApi {
        #[phoxal::output]
        report: phoxal::contracts::State<Report>,
    }

    /// The provider-side contract: only the observation import.
    #[phoxal::endpoints]
    pub struct ConsumerApi {
        #[phoxal::input(max_age_ms = 500)]
        report: Latest<Report>,
    }
}

pub(crate) struct Exporter {
    value: u64,
}

#[phoxal::runtime(contract = reverse::ExporterApi, period_ms = 10)]
impl Exporter {
    #[init]
    fn new(_config: ()) -> crate::Result<Self> {
        Ok(Self { value: 40 })
    }

    #[step]
    fn advance(&mut self, _ctx: &mut crate::runtime::Context<'_, Self>) -> crate::Result<()> {
        self.value = self.value.saturating_add(1);
        Ok(())
    }

    #[publish(report)]
    pub(crate) fn report(&self) -> reverse::Report {
        reverse::Report { value: self.value }
    }
}

/// What the provider-side runtime last observed from the brain's export;
/// shared with the test through a static because runtime configuration
/// must stay deserializable.
static REVERSE_SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) struct Importer;

#[phoxal::runtime(contract = reverse::ConsumerApi, period_ms = 10)]
impl Importer {
    #[init]
    fn new(_config: ()) -> crate::Result<Self> {
        Ok(Self)
    }

    #[step]
    fn advance(&mut self, ctx: &mut crate::runtime::Context<'_, Self>) -> crate::Result<()> {
        if let Some(report) = ctx.report().fresh() {
            REVERSE_SEEN.store(report.value, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }
}

fn reverse_graph_manifest() -> RuntimeLaunchManifest {
    let report_signature = SourceMethodSignature {
        endpoint: "report".to_owned(),
        service: "phoxal.runtime.reverse.v1.Report".to_owned(),
        method: "report".to_owned(),
        shape: crate::artifact::MethodShape::Observation,
        request: "google.protobuf.Empty".to_owned(),
        response: "phoxal.runtime.reverse.v1.Report".to_owned(),
        retained_latest: true,
        lease_valid_for_ms: None,
    };
    let exporter = SourceRuntimeRecord {
        period_ms: Some(10),
        timeout_ms: Some(100),
        init_timeout_ms: Some(100),
        inputs: vec![],
        transient_outputs: vec![SourceOutputRecord {
            name: "report".to_owned(),
            role: "method".to_owned(),
            port: Some("report".to_owned()),
            signature: Some(report_signature),
            input: None,
            max_items: None,
            max_bytes: Some(4096),
            max_request_bytes: None,
        }],
        service_outputs: vec![],
    };
    let importer = SourceRuntimeRecord {
        period_ms: Some(10),
        timeout_ms: Some(100),
        init_timeout_ms: Some(100),
        inputs: vec![SourceInputRecord {
            name: "report".to_owned(),
            role: "observation_latest".to_owned(),
            max_items: None,
            max_bytes: Some(4096),
            port: Some("report".to_owned()),
            signature: None,
        }],
        transient_outputs: vec![],
        service_outputs: vec![],
    };
    RuntimeLaunchManifest {
        root: PathBuf::from("."),
        robot_id: "reverse-graph-test".to_owned(),
        instance_id: "importer".to_owned(),
        executable: PathBuf::from("reverse-graph-test"),
        executable_sha256: "00".repeat(32),
        config: Value::Object(serde_json::Map::new()),
        connections: BTreeMap::from([(
            "importer.report".to_owned(),
            vec!["exporter.report".to_owned()],
        )]),
        requirement_destinations: BTreeMap::new(),
        observation_providers: BTreeMap::new(),
        scenario_producers: BTreeMap::new(),
        artifacts: BTreeMap::from([
            ("exporter".to_owned(), exporter),
            ("importer".to_owned(), importer),
        ]),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn brain_export_feeds_a_provider_input_through_the_graph() -> crate::Result<()> {
    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("reverse-graph")?,
            Vec::new(),
        ),
    )
    .await?;
    REVERSE_SEEN.store(0, std::sync::atomic::Ordering::Relaxed);
    let mut importer_input = ExecutionInputAdapter::<
        crate::runtime::runner::tests::phoxal_runtime_importer::Adapter,
    >::unbound();
    let mut importer_output = ExecutionOutputAdapter::<
        crate::runtime::runner::tests::phoxal_runtime_importer::Adapter,
    >::unbound();
    let mut exporter_input = ExecutionInputAdapter::<
        crate::runtime::runner::tests::phoxal_runtime_exporter::Adapter,
    >::unbound();
    let mut exporter_output = ExecutionOutputAdapter::<
        crate::runtime::runner::tests::phoxal_runtime_exporter::Adapter,
    >::unbound();
    let mut manifest = reverse_graph_manifest();
    importer_input.bind(bus.clone(), &manifest).await?;
    importer_output
        .bind(bus.clone(), &manifest.instance_id, &manifest)
        .await?;
    manifest.instance_id = "exporter".to_owned();
    exporter_input.bind(bus.clone(), &manifest).await?;
    exporter_output
        .bind(bus.clone(), &manifest.instance_id, &manifest)
        .await?;
    let mut importer = RuntimeRunner::new(
        crate::runtime::runner::tests::phoxal_runtime_importer::Adapter::new(),
        ExecutionTime::default(),
        (),
        importer_input,
        importer_output,
    )?;
    let mut exporter = RuntimeRunner::new(
        crate::runtime::runner::tests::phoxal_runtime_exporter::Adapter::new(),
        ExecutionTime::default(),
        (),
        exporter_input,
        exporter_output,
    )?;

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut tick = 0_u64;
    loop {
        tick = tick.saturating_add(1);
        exporter.poll(ExecutionTime::from_nanos(tick * 10_000_000))?;
        importer.poll(ExecutionTime::from_nanos(tick * 10_000_000 + 5_000_000))?;
        let observed = REVERSE_SEEN.load(std::sync::atomic::Ordering::Relaxed);
        if (41..=50).contains(&observed) {
            assert!(
                observed >= 41,
                "the provider observed the brain's export: {observed}"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the brain export never reached the provider's input"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    importer.stop()?;
    exporter.stop()?;
    owner.close().await;
    crate::Result::Ok(())
}

// ---------------------------------------------------------------------------
// Cancellation followed by late results, drained at scale: retired calls'
// replies must be dropped with their charges released, never retained.
// ---------------------------------------------------------------------------

#[phoxal::messages(package = "phoxal.runtime.cycle.v1")]
mod cycle {
    pub struct AskRequest {
        #[phoxal(tag = 1)]
        pub value: u64,
    }

    pub struct AskResponse {
        #[phoxal(tag = 1)]
        pub value: u64,
    }

    #[phoxal::endpoints]
    pub struct CycleApi {
        #[phoxal::call]
        ask: crate::runtime::runner::tests::CycleAsk,
    }
}

pub(crate) struct CycleAsk;

impl phoxal::contracts::Operation for CycleAsk {
    type Request = cycle::AskRequest;
    type Response = cycle::AskResponse;

    const METHOD: phoxal::contracts::CallMethod<Self::Request, Self::Response> =
        phoxal::contracts::CallMethod::new(
            "phoxal.runtime.cycle.v1.Ask",
            "Ask",
            "ask",
            "phoxal.runtime.cycle.v1.AskRequest",
            "phoxal.runtime.cycle.v1.AskResponse",
            None,
            &[],
        );
}

/// Drives alternating stage/cancel rounds: even invocations let a fresh
/// tree stage one call, odd invocations cancel it — withdrawing the
/// not-yet-accepted submission — so the provider's replies to earlier
/// accepted calls always arrive for retired tickets.
pub(crate) struct CycleBrain {
    rounds: u64,
    tree: crate::runtime::behavior::Tree<CycleBrain>,
}

impl CycleBrain {
    fn fresh_tree() -> crate::Result<crate::runtime::behavior::Tree<CycleBrain>> {
        use crate::runtime::behavior::Sequence;
        Sequence::<Self>::new()
            .call(cycle::cycle_api::calls::ask(cycle::AskRequest { value: 1 }))
            .expect_response(|response: &cycle::AskResponse| response.value == 1)
            .build()
    }
}

#[phoxal::runtime(contract = cycle::CycleApi, period_ms = 10)]
impl CycleBrain {
    #[init]
    fn new(_config: ()) -> crate::Result<Self> {
        Ok(Self {
            rounds: 0,
            tree: Self::fresh_tree()?,
        })
    }

    #[step]
    fn advance(&mut self, ctx: &mut crate::runtime::Context<'_, Self>) -> crate::Result<()> {
        if self.rounds.is_multiple_of(2) {
            self.tree.tick(ctx)?;
        } else {
            // Cancel withdraws this round's staged submission and retires
            // the tree generation's accepted calls; their late replies
            // must be refused and dropped later.
            self.tree.cancel(ctx)?;
            self.tree = Self::fresh_tree()?;
        }
        self.rounds = self.rounds.saturating_add(1);
        Ok(())
    }
}

/// Six hundred alternating stage/cancel rounds produce **three hundred**
/// accepted-then-cancelled calls in one execution — more late replies than
/// the retained mailbox's 256-item bound — so a broken release faults
/// visibly instead of hiding under the bound. This is runner-level
/// ownership evidence only; process reset is proven separately below.
#[allow(
    clippy::type_complexity,
    reason = "one assembled two-runner cycle graph"
)]
async fn cycle_graph() -> crate::Result<(
    crate::runtime::connection::ConnectionOwner,
    RuntimeRunner<
        crate::runtime::runner::tests::phoxal_runtime_cycle_brain::Adapter,
        ExecutionInputAdapter<crate::runtime::runner::tests::phoxal_runtime_cycle_brain::Adapter>,
        ExecutionOutputAdapter<crate::runtime::runner::tests::phoxal_runtime_cycle_brain::Adapter>,
    >,
    RuntimeRunner<
        CountingProviderRuntime,
        ExecutionInputAdapter<CountingProviderRuntime>,
        ExecutionOutputAdapter<CountingProviderRuntime>,
    >,
    Arc<std::sync::atomic::AtomicUsize>,
)> {
    let (owner, bus) = crate::runtime::connection::ConnectionOwner::open(
        crate::runtime::connection::ConnectionConfig::for_participant(
            crate::identity::ExecutionId::mint(),
            crate::identity::ParticipantId::new("cycle-graph")?,
            Vec::new(),
        ),
    )
    .await?;
    let signature = SourceMethodSignature {
        endpoint: TRANSPORT_PORT.name.to_owned(),
        service: TRANSPORT_PORT.service.to_owned(),
        method: TRANSPORT_PORT.method.to_owned(),
        shape: crate::artifact::MethodShape::Call,
        request: TRANSPORT_PORT.request.to_owned(),
        response: TRANSPORT_PORT.response.to_owned(),
        retained_latest: false,
        lease_valid_for_ms: None,
    };
    let consumer_record = SourceRuntimeRecord {
        period_ms: Some(10),
        timeout_ms: Some(100),
        init_timeout_ms: Some(100),
        inputs: vec![SourceInputRecord {
            name: "ask".to_owned(),
            role: "call_completions".to_owned(),
            max_items: None,
            max_bytes: None,
            port: Some(TRANSPORT_PORT.name.to_owned()),
            signature: Some(signature),
        }],
        transient_outputs: vec![],
        service_outputs: vec![],
    };
    let mut manifest = RuntimeLaunchManifest {
        root: PathBuf::from("."),
        robot_id: "cycle-graph-test".to_owned(),
        instance_id: "cycle".to_owned(),
        executable: PathBuf::from("cycle-graph-test"),
        executable_sha256: "00".repeat(32),
        config: Value::Object(serde_json::Map::new()),
        connections: BTreeMap::from([(
            "cycle.ask".to_owned(),
            vec![format!("countdown.{}", TRANSPORT_PORT.name)],
        )]),
        requirement_destinations: BTreeMap::from([(
            "ask".to_owned(),
            ("countdown".to_owned(), TRANSPORT_PORT),
        )]),
        observation_providers: BTreeMap::new(),
        scenario_producers: BTreeMap::new(),
        artifacts: BTreeMap::from([("cycle".to_owned(), consumer_record)]),
    };
    let server_record = generated_call_manifest()
        .artifacts
        .remove("server")
        .expect("the shared server record exists");
    manifest
        .artifacts
        .insert("countdown".to_owned(), server_record.clone());
    let generated_correlations = Arc::new(Mutex::new(BTreeMap::new()));
    let generated_completions = Arc::new(Mutex::new(Vec::new()));
    let mut consumer_input = ExecutionInputAdapter::<
        crate::runtime::runner::tests::phoxal_runtime_cycle_brain::Adapter,
    >::unbound()
    .with_generated_calls(
        Arc::clone(&generated_correlations),
        Arc::clone(&generated_completions),
    );
    let mut consumer_output = ExecutionOutputAdapter::<
        crate::runtime::runner::tests::phoxal_runtime_cycle_brain::Adapter,
    >::unbound()
    .with_generated_calls(generated_correlations, generated_completions);
    consumer_input.bind(bus.clone(), &manifest).await?;
    consumer_output
        .bind(bus.clone(), &manifest.instance_id, &manifest)
        .await?;
    let consumer = RuntimeRunner::new(
        crate::runtime::runner::tests::phoxal_runtime_cycle_brain::Adapter::new(),
        ExecutionTime::default(),
        (),
        consumer_input,
        consumer_output,
    )?;

    manifest.instance_id = "countdown".to_owned();
    manifest
        .artifacts
        .insert("countdown".to_owned(), server_record);
    let mut provider_input = ExecutionInputAdapter::<CountingProviderRuntime>::unbound();
    let mut provider_output = ExecutionOutputAdapter::<CountingProviderRuntime>::unbound();
    provider_input.bind(bus.clone(), &manifest).await?;
    provider_output
        .bind(bus.clone(), &manifest.instance_id, &manifest)
        .await?;
    let handled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider = RuntimeRunner::new(
        CountingProviderRuntime {
            handled: Arc::clone(&handled),
            offset: 1,
        },
        ExecutionTime::default(),
        (),
        provider_input,
        provider_output,
    )?;
    Ok((owner, consumer, provider, handled))
}

// Real-time soak over a live connection: each tick waits for real delivery
// between polls, so it runs in the integration lane (`e2e` feature).
#[cfg(feature = "e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_call_late_replies_drain_beyond_mailbox_capacity() -> crate::Result<()> {
    const ROUNDS: u64 = 600;
    const EXPECTED_ACCEPTED: usize = 300;
    let (owner, mut consumer, mut provider, handled) = cycle_graph().await?;

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut tick = 0_u64;
    loop {
        tick = tick.saturating_add(1);
        provider.poll(ExecutionTime::from_nanos(tick * 10_000_000))?;
        consumer.poll(ExecutionTime::from_nanos(tick * 10_000_000 + 5_000_000))?;
        if tick >= ROUNDS {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the cancellation drain stalled at tick {tick}/{ROUNDS}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    // Every accepted submission was served exactly once, and their three
    // hundred late replies — more than the mailbox's 256-item bound — were
    // each dropped with their charges released: nothing exhausted.
    assert_eq!(
        handled.load(std::sync::atomic::Ordering::Relaxed),
        EXPECTED_ACCEPTED,
        "exactly the accepted stage-round submissions were served"
    );
    consumer.stop()?;
    provider.stop()?;
    owner.close().await;
    crate::Result::Ok(())
}

/// Runner-level reset in one process: the fresh execution draws a fresh
/// epoch, fences every retired ticket of the old one, and tree-driven
/// cancellation cycles continue on both sides of the boundary. This is
/// `RuntimeRunner::reset` evidence, not a supervisor/process reset.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runner_reset_fences_retired_calls_across_the_boundary() -> crate::Result<()> {
    const ROUNDS: u64 = 64;
    let (owner, mut consumer, mut provider, _handled) = cycle_graph().await?;

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut tick = 0_u64;
    loop {
        tick = tick.saturating_add(1);
        provider.poll(ExecutionTime::from_nanos(tick * 10_000_000))?;
        consumer.poll(ExecutionTime::from_nanos(tick * 10_000_000 + 5_000_000))?;
        if tick == ROUNDS / 2 {
            // The boundary under test: reinitialize the consumer's
            // execution mid-cycling and keep draining.
            consumer.reset(ExecutionTime::from_nanos(tick * 10_000_000), ())?;
        }
        if tick >= ROUNDS {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the reset drain stalled at tick {tick}/{ROUNDS}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    consumer.stop()?;
    provider.stop()?;
    owner.close().await;
    crate::Result::Ok(())
}
