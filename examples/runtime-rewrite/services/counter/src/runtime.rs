use crate::{config::CounterConfig, inputs::CounterInputs, outputs::CounterOutputs};
use phoxal::runtime::{InitContext, Runtime, StepContext};

/// A counter service that advances once per invocation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Counter;

#[phoxal::runtime(period_ms = 100, timeout_ms = 50, init_timeout_ms = 1_000)]
impl Runtime for Counter {
    type Config = CounterConfig;
    type State = u64;
    type Inputs = CounterInputs;
    type Outputs = CounterOutputs;

    fn init(&self, _ctx: &InitContext, config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(config.initial)
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        _inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        Ok((state.saturating_add(1), CounterOutputs {}))
    }
}

#[cfg(test)]
mod tests {
    use super::{Counter, CounterConfig, CounterInputs};
    use example_counter_service::counter;
    use phoxal::runtime::{ExecutionDuration, ExecutionTime, StepContext, initialize, invoke};

    #[test]
    fn direct_runtime_example_uses_the_real_api() {
        let runtime = Counter;
        let state = initialize(
            &runtime,
            ExecutionTime::default(),
            CounterConfig { initial: 4 },
        )
        .expect("counter initialization");
        let context = StepContext::first(
            ExecutionTime::from_nanos(100_000_000),
            ExecutionDuration::from_millis(100),
        );
        let (state, _) =
            invoke(&runtime, &context, state, &CounterInputs {}).expect("counter invocation");
        assert_eq!(state, 5);
    }

    #[test]
    fn owner_contract_keeps_the_generated_state_port() {
        assert_eq!(counter::STATE.name(), "state");
        assert_eq!(
            counter::STATE.signature().service,
            "example.counter.v1.Counter"
        );
        assert!(!counter_contract::FILE_DESCRIPTOR_SET.is_empty());
    }
}
