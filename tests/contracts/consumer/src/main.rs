//! Manifest-authored consumer fixture: every endpoint comes from
//! `service.yaml`; this file owns only configuration, state, and behavior.

phoxal::api!();

use crate::api::types::example::contract_evaluation::v1::ConsumerStatus;
use phoxal::contract::Empty;
use phoxal::robotics::EncoderSample;
use phoxal::runtime::{CallTicket, InitContext, Runtime, StepContext};

#[derive(Clone, Debug, Default, serde::Deserialize, phoxal::Config)]
struct ConsumerConfig {
    /// Phase reported after the first fresh encoder observation arrives.
    #[serde(default = "default_ready_phase")]
    ready_phase: String,
}

fn default_ready_phase() -> String {
    "running".to_owned()
}

#[derive(Debug, Default)]
struct ConsumerState {
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
    pending_read: Option<CallTicket<EncoderSample>>,
    pending_backup: Option<CallTicket<EncoderSample>>,
}

struct Consumer;

fn status(state: &ConsumerState) -> ConsumerStatus {
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

fn observe(state: &mut ConsumerState, inputs: &<Consumer as Runtime>::Inputs, ctx: &StepContext) {
    // Freshness gates the ready phase through the generated helper, which
    // applies the declared 100 ms bound; hardware-local clocks share the
    // wall-clock epoch, so a foreign stamp evaluates against this runtime's
    // now within the bounded-skew policy.  Absence and staleness are both
    // deliberate observable states: the previous position is kept and the
    // phase reports waiting rather than treating stale data as current.
    if !inputs.encoder_fresh(ctx.now()) {
        state.phase = "waiting".to_owned();
        return;
    }
    if let Some(sample) = inputs.encoder.sample()
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
}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Consumer {
    type Config = ConsumerConfig;
    type State = ConsumerState;

    fn init(&self, _ctx: &InitContext, config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(ConsumerState {
            ready_phase: config.ready_phase,
            ..ConsumerState::default()
        })
    }

    fn step(
        &self,
        ctx: &StepContext,
        mut state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        observe(&mut state, inputs, ctx);

        // Queued data is required: an overflow that dropped records is a
        // visible rejection, never a silent prefix.
        if inputs.ticks.has_gap() {
            return Err(phoxal::anyhow!(
                "queued ticks overflowed the declared 4-item bound"
            ));
        }
        for tick in inputs.ticks.items() {
            tick.payload()
                .validate()
                .map_err(|error| phoxal::anyhow!(error))?;
            state.ticks = state.ticks.saturating_add(1);
        }

        // Complete staged readings; failures are counted, never fabricated
        // into successful readings.  The primary and backup requirements
        // keep independent tickets on their own completion fields.
        if let Some(ticket) = state.pending_read.take() {
            match inputs.read_encoder.get(&ticket) {
                Some(completion) => match completion.response() {
                    Ok(sample) => {
                        sample.validate().map_err(|error| phoxal::anyhow!(error))?;
                        state.readings = state.readings.saturating_add(1);
                        state.last_position = sample.position_rad;
                    }
                    Err(_) => {
                        state.failed_reads = state.failed_reads.saturating_add(1);
                    }
                },
                None => state.pending_read = Some(ticket),
            }
        }
        if let Some(ticket) = state.pending_backup.take() {
            match inputs.read_backup.get(&ticket) {
                Some(completion) => match completion.response() {
                    Ok(sample) => {
                        sample.validate().map_err(|error| phoxal::anyhow!(error))?;
                        state.backups = state.backups.saturating_add(1);
                        state.backup_position = sample.position_rad;
                    }
                    Err(_) => {
                        state.failed_reads = state.failed_reads.saturating_add(1);
                    }
                },
                None => state.pending_backup = Some(ticket),
            }
        }

        let mut outputs = Self::Outputs::default();
        outputs.status(status(&state))?;
        for request in inputs.inspect.items() {
            outputs.inspect_reply(request.reply(status(&state)))?;
        }

        // Stage the next composition-bound readings; the provider instances
        // are resolved from this service's own robot connections at
        // execution.  A runtime caller paces itself well below the
        // provider's declared ingress bound: boundary counters drift
        // between independent runtimes, so per-step calling can overflow
        // the receiver.
        state.steps = state.steps.saturating_add(1);
        const READ_WARMUP_STEPS: u64 = 10;
        const READ_EVERY_STEPS: u64 = 25;
        if state.steps >= READ_WARMUP_STEPS
            && (state.steps - READ_WARMUP_STEPS).is_multiple_of(READ_EVERY_STEPS)
        {
            if state.pending_read.is_none() {
                let ticket = outputs.send(ctx, crate::api::calls::read_encoder(Empty {}))?;
                state.pending_read = Some(ticket);
            }
            if state.pending_backup.is_none() {
                let ticket = outputs.send(ctx, crate::api::calls::read_backup(Empty {}))?;
                state.pending_backup = Some(ticket);
            }
        }
        Ok((state, outputs))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(Consumer)
}

/// Owner-level acceptance through the generated endpoint surface: typed
/// init/step, serialized invocation acceptance, and capacity rollback.
#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::runtime::input::{Commands, Completions, Latest, Samples};
    use phoxal::runtime::{
        ExecutionDuration, ExecutionTime, ObservationStamp, OutputAdmission, RuntimeOwner,
        RuntimeStatus, initialize, invoke,
    };

    type ConsumerInputs = <Consumer as Runtime>::Inputs;

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
            &Consumer,
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
        let (state, outputs) =
            invoke(&Consumer, &context, state, &unavailable_inputs()).expect("step");
        assert_eq!(state.phase, "waiting");
        assert_eq!(outputs.status.as_ref().expect("status").phase, "waiting");
    }

    #[test]
    fn owner_serializes_acceptance_from_generated_inputs_and_outputs() {
        let mut owner = RuntimeOwner::new(
            Consumer,
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

    impl OutputAdmission<<Consumer as Runtime>::Outputs> for RejectOutputs {
        type Reservation = ();

        fn reserve(
            &mut self,
            _outputs: &<Consumer as Runtime>::Outputs,
        ) -> phoxal::Result<Self::Reservation> {
            Err(phoxal::anyhow!("fixture capacity exhausted"))
        }
    }

    #[test]
    fn output_capacity_is_reserved_before_invocation_acceptance() {
        let mut owner = RuntimeOwner::new(
            Consumer,
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
