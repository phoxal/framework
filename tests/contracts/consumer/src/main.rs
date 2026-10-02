//! Rust-authored consumer fixture: endpoints and payloads are declared once
//! in this file; behavior owns only configuration, state, and stepping.

use phoxal::contracts::component::encoder::EncoderSample;
use phoxal::contracts::{Empty, Latest, Queue, RequestReply};
use phoxal::runtime::{CallCompletion, Context};

#[phoxal::message(package = "example.contract_evaluation.v1")]
pub struct ConsumerStatus {
    /// Lifecycle phase reported by the consumer.
    #[phoxal(tag = 1)]
    pub phase: String,
    /// Count of accepted encoder observations since initialization.
    #[phoxal(tag = 2)]
    pub observed: u64,
    /// Count of completed read_encoder calls since initialization.
    #[phoxal(tag = 3)]
    pub readings: u64,
    /// Last encoder position reported by the current provider, if any.
    #[phoxal(tag = 4)]
    pub position_rad: Option<f64>,
    /// Count of accepted queued tick batches since initialization.
    #[phoxal(tag = 5)]
    pub ticks: u64,
    /// Count of completed read_backup calls since initialization.
    #[phoxal(tag = 6)]
    pub backups: u64,
    /// Last encoder position reported by the backup provider, if any.
    #[phoxal(tag = 7)]
    pub backup_position_rad: Option<f64>,
}

/// The consumer's endpoint contract.
#[phoxal::endpoints]
pub struct ConsumerApi {
    #[phoxal::input(max_age_ms = 100, max_bytes = 1024)]
    encoder: Latest<EncoderSample>,

    #[phoxal::input(max_items = 4, max_bytes = 4096)]
    ticks: Queue<EncoderSample>,

    #[phoxal::output(max_bytes = 4096)]
    status: Latest<ConsumerStatus>,

    #[phoxal::operation(
        contract = "example.contract_evaluation.v1.InspectConsumer",
        max_items = 8,
        max_bytes = 4096
    )]
    inspect: RequestReply<Empty, ConsumerStatus>,

    #[phoxal::call(
        contract = "example.contract_evaluation.v1.ReadEncoder",
        max_items = 8,
        max_bytes = 1024
    )]
    read_encoder: RequestReply<Empty, EncoderSample>,

    #[phoxal::call(
        contract = "example.contract_evaluation.v1.ReadEncoder",
        max_items = 8,
        max_bytes = 1024
    )]
    read_backup: RequestReply<Empty, EncoderSample>,
}

#[derive(Clone, Debug, Default, serde::Deserialize, phoxal::Config)]
struct ConsumerConfig {
    /// Phase reported after the first fresh encoder observation arrives.
    #[serde(default = "default_ready_phase")]
    ready_phase: String,
}

fn default_ready_phase() -> String {
    "running".to_owned()
}

#[derive(Debug)]
struct Consumer {
    ready_phase: String,
    phase: String,
    steps: u64,
    observed: u64,
    ticks: u64,
    readings: u64,
    backups: u64,
    failed_reads: u64,
    last_position: Option<f64>,
    backup_position: Option<f64>,
    read_in_flight: bool,
    backup_in_flight: bool,
    applied_invocation: u64,
}

fn status(state: &Consumer) -> ConsumerStatus {
    ConsumerStatus {
        phase: if state.phase.is_empty() {
            "starting".to_owned()
        } else {
            state.phase.clone()
        },
        observed: state.observed,
        readings: state.readings,
        ticks: state.ticks,
        backups: state.backups,
        position_rad: state.last_position,
        backup_position_rad: state.backup_position,
    }
}

/// Observes the encoder input and the queued tick batch exactly once per
/// invocation, so an inspect dispatched before the periodic step reports
/// the same invocation state the unified step observed.
fn observe(state: &mut Consumer, ctx: &Context<'_, Consumer>) -> phoxal::Result<()> {
    if state.applied_invocation == ctx.invocation_index() {
        return Ok(());
    }
    state.applied_invocation = ctx.invocation_index();
    // Freshness gates the ready phase through the declared 100 ms bound;
    // hardware-local clocks share the wall-clock epoch, so a foreign stamp
    // evaluates against this runtime's now within the bounded-skew policy.
    // Absence and staleness are both deliberate observable states: the
    // previous position is kept and the phase reports waiting rather than
    // treating stale data as current.
    if !ctx.encoder().is_fresh() {
        state.phase = "waiting".to_owned();
    } else if let Some(sample) = ctx.encoder().sample()
        && sample.payload().validate().is_ok()
    {
        state.observed = state.observed.saturating_add(1);
        state.last_position = sample.payload().position_rad;
        state.phase = if state.ready_phase.is_empty() {
            "running".to_owned()
        } else {
            state.ready_phase.clone()
        };
    } else {
        state.phase = "waiting".to_owned();
    }

    // Queued data is required: an overflow that dropped records is a
    // visible rejection, never a silent prefix.
    if ctx.ticks().has_gap() {
        return Err(phoxal::anyhow!(
            "queued ticks overflowed the declared 4-item bound"
        ));
    }
    for tick in ctx.ticks().items() {
        tick.payload()
            .validate()
            .map_err(|error| phoxal::anyhow!(error))?;
        state.ticks = state.ticks.saturating_add(1);
    }
    Ok(())
}

#[phoxal::runtime(contract = ConsumerApi, period_ms = 20)]
impl Consumer {
    #[init]
    fn new(config: ConsumerConfig) -> phoxal::Result<Self> {
        Ok(Self {
            ready_phase: config.ready_phase,
            phase: String::new(),
            steps: 0,
            observed: 0,
            ticks: 0,
            readings: 0,
            backups: 0,
            failed_reads: 0,
            last_position: None,
            backup_position: None,
            read_in_flight: false,
            backup_in_flight: false,
            applied_invocation: u64::MAX,
        })
    }

    /// Reports the consumer's status from this invocation's observed state.
    #[handle(inspect)]
    fn inspect(
        &mut self,
        ctx: &mut Context<'_, Self>,
        _request: Empty,
    ) -> phoxal::Result<ConsumerStatus> {
        observe(self, ctx)?;
        Ok(status(self))
    }

    /// Completes one staged primary reading; failures are counted, never
    /// fabricated into successful readings.
    #[complete(read_encoder)]
    fn read_completed(
        &mut self,
        _ctx: &mut Context<'_, Self>,
        completion: CallCompletion<EncoderSample>,
    ) -> phoxal::Result<()> {
        self.read_in_flight = false;
        self.complete_reading(completion, false)
    }

    /// Completes one staged backup reading on its own completion field.
    #[complete(read_backup)]
    fn backup_completed(
        &mut self,
        _ctx: &mut Context<'_, Self>,
        completion: CallCompletion<EncoderSample>,
    ) -> phoxal::Result<()> {
        self.backup_in_flight = false;
        self.complete_reading(completion, true)
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
        observe(self, ctx)?;
        ctx.publish_status(status(self))?;

        // Stage the next composition-bound readings; the provider instances
        // are resolved from this service's own robot connections at
        // execution.  A runtime caller paces itself well below the
        // provider's declared ingress bound: boundary counters drift
        // between independent runtimes, so per-step calling can overflow
        // the receiver.
        self.steps = self.steps.saturating_add(1);
        const READ_WARMUP_STEPS: u64 = 10;
        const READ_EVERY_STEPS: u64 = 25;
        if self.steps >= READ_WARMUP_STEPS
            && (self.steps - READ_WARMUP_STEPS).is_multiple_of(READ_EVERY_STEPS)
        {
            if !self.read_in_flight {
                ctx.read_encoder(Empty {})?;
                self.read_in_flight = true;
            }
            if !self.backup_in_flight {
                ctx.read_backup(Empty {})?;
                self.backup_in_flight = true;
            }
        }
        Ok(())
    }

    fn complete_reading(
        &mut self,
        completion: CallCompletion<EncoderSample>,
        backup: bool,
    ) -> phoxal::Result<()> {
        match completion.into_result() {
            Ok(sample) => {
                sample.validate().map_err(|error| phoxal::anyhow!(error))?;
                if backup {
                    self.backups = self.backups.saturating_add(1);
                    self.backup_position = sample.position_rad;
                } else {
                    self.readings = self.readings.saturating_add(1);
                    self.last_position = sample.position_rad;
                }
            }
            Err(_) => {
                self.failed_reads = self.failed_reads.saturating_add(1);
            }
        }
        Ok(())
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run::<Consumer>()
}

/// Owner-level acceptance through the generated endpoint surface: typed
/// init/step, serialized invocation acceptance, and capacity rollback.
#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::runtime::StepContext;
    use phoxal::runtime::input::{Commands, Completions, Latest, Samples};
    use phoxal::runtime::{
        ExecutionDuration, ExecutionTime, ObservationStamp, OutputAdmission, RuntimeOwner,
        RuntimeStatus, initialize, invoke,
    };

    type ConsumerInputs = consumer_api::Inputs;

    fn unavailable_inputs() -> ConsumerInputs {
        ConsumerInputs {
            encoder: Latest::unavailable(),
            ticks: Samples::default(),
            inspect: Commands::default(),
            read_encoder: Completions::default(),
            read_backup: Completions::default(),
        }
    }

    #[test]
    fn direct_adapter_uses_typed_init_and_step() {
        let state = initialize(
            &super::phoxal_runtime_consumer::Adapter::new(),
            ExecutionTime::from_nanos(0),
            ConsumerConfig {
                ready_phase: "cruising".to_owned(),
            },
        )
        .expect("config and init");
        assert_eq!(state.ready_phase, "cruising");
        let context = StepContext::first(
            ExecutionTime::from_nanos(20_000_000),
            ExecutionDuration::from_millis(20),
        );
        let (state, outputs) = invoke(
            &super::phoxal_runtime_consumer::Adapter::new(),
            &context,
            state,
            &unavailable_inputs(),
        )
        .expect("step");
        assert_eq!(state.phase, "waiting");
        assert_eq!(outputs.status.as_ref().expect("status").phase, "waiting");
    }

    /// Qualifies the authored dispatch order explicitly: within one
    /// invocation, the merged command dispatch (inspect) runs before the
    /// direct completion handlers, which run before the periodic step.
    /// An inspect merged with its survey reply therefore reports the
    /// state before that reply's reading lands.
    #[test]
    fn an_inspect_merged_with_its_reply_reports_the_pre_completion_state() {
        use phoxal::contracts::ProstPayload;
        use phoxal::runtime::StepContext;
        use phoxal::runtime::input::{
            Command, CommandId, CommandOrder, TransportCallCompletion, TransportInputSink,
        };

        let adapter = super::phoxal_runtime_consumer::Adapter::new();
        let epoch = adapter.execution_epoch();
        let mut service = initialize(
            &adapter,
            ExecutionTime::default(),
            ConsumerConfig::default(),
        )
        .expect("initialize consumer");

        // Warm up so a survey is staged (warmup 10, every 25 steps).
        for index in 0..10 {
            let context = StepContext::from_previous(
                ExecutionTime::from_nanos(index * 20_000_000),
                ExecutionDuration::from_millis(20),
                Some(ExecutionTime::from_nanos(
                    index.saturating_sub(1) * 20_000_000,
                )),
                0,
                index,
            );
            let (_service, _) =
                invoke(&adapter, &context, service, &unavailable_inputs()).expect("warmup step");
            service = _service;
        }
        let _ = epoch;

        // Invocation 10 stages the primary and backup surveys; invocation
        // 11 merges one inspect with the primary reply. The inspect runs
        // first in the merged dispatch and must report the pre-reply
        // reading count; the reply lands afterwards in the same
        // invocation.
        let staging = StepContext::from_previous(
            ExecutionTime::from_nanos(200_000_000),
            ExecutionDuration::from_millis(20),
            Some(ExecutionTime::from_nanos(180_000_000)),
            0,
            10,
        );
        let (service, _) =
            invoke(&adapter, &staging, service, &unavailable_inputs()).expect("staging step");

        let ticket = phoxal::runtime::outputs::compose_call_ticket(epoch, 10, 0)
            .expect("ticket space representable");
        let mut merged = unavailable_inputs();
        merged.inspect = Commands::new(vec![Command::with_order(
            CommandOrder::new(1, 0, CommandId::new(9)),
            Empty {},
        )]);
        merged
            .set_call_completions(vec![TransportCallCompletion {
                ticket,
                result: Ok(EncoderSample {
                    position_rad: Some(4.0),
                    velocity_radps: Some(0.0),
                }
                .encode_payload()
                .expect("encode")),
            }])
            .expect("completion accepted");
        let observing = StepContext::from_previous(
            ExecutionTime::from_nanos(220_000_000),
            ExecutionDuration::from_millis(20),
            Some(ExecutionTime::from_nanos(200_000_000)),
            0,
            11,
        );
        let (_service, outputs) =
            invoke(&adapter, &observing, service, &merged).expect("merged invocation");
        let reported = outputs.inspect_replies[0].response();
        assert_eq!(
            reported.readings, 0,
            "the merged inspect observed the pre-completion state"
        );
        assert_eq!(
            reported.position_rad, None,
            "the reply's reading lands after the inspect in the same invocation"
        );
    }

    #[test]
    fn owner_serializes_acceptance_from_generated_inputs_and_outputs() {
        let mut owner = RuntimeOwner::new(
            super::phoxal_runtime_consumer::Adapter::new(),
            ExecutionTime::from_nanos(0),
            ConsumerConfig::default(),
        )
        .expect("owner initialization");
        assert_eq!(owner.status(), RuntimeStatus::Ready);

        let first_context = StepContext::first(
            ExecutionTime::from_nanos(20_000_000),
            ExecutionDuration::from_millis(20),
        );
        let first = owner
            .accept(&first_context, &unavailable_inputs())
            .expect("first acceptance");
        assert_eq!(first.invocation().index(), 0);
        let waiting = first.outputs().status.as_ref().expect("status");
        assert_eq!(waiting.phase, "waiting");
        assert_eq!(waiting.observed, 0);

        let fresh = Latest::new(
            EncoderSample {
                position_rad: Some(1.5),
                ..EncoderSample::default()
            },
            ObservationStamp::new(
                "encoder_source",
                ExecutionTime::from_nanos(40_000_000),
                None,
            ),
        );
        let second_inputs = ConsumerInputs {
            encoder: fresh,
            ..unavailable_inputs()
        };
        let second_context = StepContext::from_previous(
            ExecutionTime::from_nanos(40_000_000),
            ExecutionDuration::from_millis(20),
            Some(first_context.now()),
            0,
            1,
        );
        let second = owner
            .accept(&second_context, &second_inputs)
            .expect("second acceptance");
        assert_eq!(second.invocation().index(), 1);
        let running = second.outputs().status.as_ref().expect("status");
        assert_eq!(running.phase, "running");
        assert_eq!(running.observed, 1);
        assert_eq!(running.position_rad, Some(1.5));
        assert_eq!(owner.next_invocation().index(), 2);
    }

    struct RejectOutputs;

    impl OutputAdmission<consumer_api::Outputs> for RejectOutputs {
        type Reservation = ();

        fn reserve(
            &mut self,
            _outputs: &consumer_api::Outputs,
        ) -> phoxal::Result<Self::Reservation> {
            Err(phoxal::anyhow!("fixture capacity exhausted"))
        }
    }

    #[test]
    fn output_capacity_is_reserved_before_invocation_acceptance() {
        let mut owner = RuntimeOwner::new(
            super::phoxal_runtime_consumer::Adapter::new(),
            ExecutionTime::from_nanos(0),
            ConsumerConfig::default(),
        )
        .expect("owner initialization");
        let context = StepContext::first(
            ExecutionTime::from_nanos(20_000_000),
            ExecutionDuration::from_millis(20),
        );
        let error = match owner.accept_with(&context, &unavailable_inputs(), &mut RejectOutputs) {
            Ok(_) => panic!("capacity rejection must reject the complete candidate"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("capacity exhausted"));
        assert_eq!(owner.status(), RuntimeStatus::Failed);
        assert_eq!(owner.next_invocation().index(), 0);
    }
}
