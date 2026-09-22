use phoxal::runtime::input::{Events, Latest};
use phoxal::runtime::{
    ExecutionDuration, ExecutionTime, InitContext, ObservationStamp, OutputAdmission, Runtime,
    RuntimeOwner, RuntimeStatus, StepContext, initialize, invoke,
};

struct Counter;

#[derive(serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
struct CounterConfig {
    #[serde(default)]
    initial: u64,
}

#[phoxal::runtime::inputs]
#[allow(dead_code)]
struct CounterInputs {
    #[phoxal::runtime::input(max_age_ms = 100)]
    latest: Latest<u64>,
    #[phoxal::runtime::input(max_items = 4, max_bytes = 64)]
    events: Events<u32>,
}

#[derive(Default)]
#[phoxal::runtime::outputs]
#[allow(dead_code)]
struct CounterOutputs {
    #[phoxal::runtime::outputs::event(
        port = COUNTER_EVENTS,
        max_items = 4,
        max_bytes = 64,
    )]
    events: Vec<u32>,
}

const COUNTER_STATE: phoxal::__private::State<u64> = phoxal::__private::State::new("state");
const COUNTER_EVENTS: phoxal::__private::Event<u32> = phoxal::__private::Event::new("events");

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1000)]
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
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        let latest = inputs.latest.value().copied().unwrap_or_default();
        let next = state + latest;
        let mut outputs = CounterOutputs::default();
        outputs.events.push(next as u32);
        Ok((next, outputs))
    }
}

#[phoxal::runtime::outputs]
#[allow(dead_code)]
impl Counter {
    #[phoxal::runtime::outputs::state(
        port = COUNTER_STATE,
        max_bytes = 64,
        every_steps = 2,
        bootstrap,
        on_change,
    )]
    fn state(&self, state: &u64) -> u64 {
        *state
    }
}

#[test]
fn direct_adapter_uses_typed_init_and_step() {
    let state = initialize(
        &Counter,
        ExecutionTime::from_nanos(0),
        CounterConfig { initial: 7 },
    )
    .expect("config and init");
    let inputs = CounterInputs {
        latest: Latest::unavailable(),
        events: Events::default(),
    };
    let context = StepContext::first(
        ExecutionTime::from_nanos(20_000_000),
        ExecutionDuration::from_millis(20),
    );
    let (state, _outputs) = invoke(&Counter, &context, state, &inputs).expect("step");
    assert_eq!(state, 7);
}

#[test]
fn direct_owner_serializes_state_and_acceptance() {
    let mut owner = RuntimeOwner::new(
        Counter,
        ExecutionTime::from_nanos(0),
        CounterConfig { initial: 7 },
    )
    .expect("owner initialization");
    assert_eq!(owner.status(), RuntimeStatus::Ready);

    let first_inputs = CounterInputs {
        latest: Latest::unavailable(),
        events: Events::default(),
    };
    let first_context = StepContext::first(
        ExecutionTime::from_nanos(20_000_000),
        ExecutionDuration::from_millis(20),
    );
    let first = owner
        .accept(&first_context, &first_inputs)
        .expect("first acceptance");
    assert_eq!(first.invocation().index(), 0);
    assert_eq!(first.outputs().events, [7]);

    let second_inputs = CounterInputs {
        latest: Latest::new(
            2,
            ObservationStamp::new("fixture", ExecutionTime::from_nanos(20_000_000), None),
        ),
        events: Events::default(),
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
    assert_eq!(second.outputs().events, [9]);
    assert_eq!(owner.next_invocation().index(), 2);
}

struct RejectOutputs;

impl OutputAdmission<CounterOutputs> for RejectOutputs {
    type Reservation = ();

    fn reserve(&mut self, _outputs: &CounterOutputs) -> phoxal::Result<Self::Reservation> {
        anyhow::bail!("fixture capacity exhausted")
    }
}

#[test]
fn output_capacity_is_reserved_before_invocation_acceptance() {
    let mut owner = RuntimeOwner::new(
        Counter,
        ExecutionTime::from_nanos(0),
        CounterConfig { initial: 7 },
    )
    .expect("owner initialization");
    let context = StepContext::first(
        ExecutionTime::from_nanos(20_000_000),
        ExecutionDuration::from_millis(20),
    );
    let inputs = CounterInputs {
        latest: Latest::unavailable(),
        events: Events::default(),
    };
    let error = match owner.accept_with(&context, &inputs, &mut RejectOutputs) {
        Ok(_) => panic!("capacity rejection must reject the complete candidate"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("capacity exhausted"));
    assert_eq!(owner.status(), RuntimeStatus::Failed);
    assert_eq!(owner.next_invocation().index(), 0);
}
