use phoxal::artifact::RuntimeRecord;
use phoxal::runtime::input::{Events, Read, ReadError, ReadStatus};
use phoxal::runtime::{
    Activation, ExecutionDuration, ExecutionTime, InitContext, ObservationStamp, Runtime,
    StepContext, initialize, invoke,
};

const COUNTER_STATUS: phoxal::__private::State<CounterStatus> =
    phoxal::__private::State::new("counter-status");
const COUNTER_READ: phoxal::__private::Read<CounterReadRequest, CounterReadResponse> =
    phoxal::__private::Read::new("counter-read");
const READER_STATUS: phoxal::__private::State<ReaderStatus> =
    phoxal::__private::State::new("reader-status");

#[derive(Clone, Copy, Eq, PartialEq, prost::Message)]
struct CounterStatus {
    #[prost(uint64, tag = "1")]
    inspected_count: u64,
}

impl prost::Name for CounterStatus {
    const NAME: &'static str = "CounterStatus";
    const PACKAGE: &'static str = "phoxal.tests.runtime";
}

#[derive(Clone, Copy, Eq, PartialEq, prost::Message)]
struct CounterReadRequest {
    #[prost(bool, tag = "1")]
    include_count: bool,
}

impl prost::Name for CounterReadRequest {
    const NAME: &'static str = "CounterReadRequest";
    const PACKAGE: &'static str = "phoxal.tests.runtime";
}

#[derive(Clone, Copy, Eq, PartialEq, prost::Message)]
struct CounterReadResponse {
    #[prost(uint64, tag = "1")]
    inspected_count: u64,
}

impl prost::Name for CounterReadResponse {
    const NAME: &'static str = "CounterReadResponse";
    const PACKAGE: &'static str = "phoxal.tests.runtime";
}

#[derive(Clone, Copy, Eq, PartialEq, prost::Message)]
struct ReaderStatus {
    #[prost(uint64, optional, tag = "1")]
    last_count: Option<u64>,
    #[prost(bool, tag = "2")]
    finished: bool,
    #[prost(bool, tag = "3")]
    last_attempt_failed: bool,
}

impl prost::Name for ReaderStatus {
    const NAME: &'static str = "ReaderStatus";
    const PACKAGE: &'static str = "phoxal.tests.runtime";
}

struct Counter;

#[phoxal::runtime::inputs]
struct CounterInputs {
    #[phoxal::runtime::input(max_items = 4, max_bytes = 64)]
    increments: Events<u64>,
}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1000)]
impl Runtime for Counter {
    type Config = ();
    type State = u64;
    type Inputs = CounterInputs;
    type Outputs = ();

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(0)
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        let next = inputs
            .increments
            .items()
            .iter()
            .fold(state, |total, increment| total.saturating_add(*increment));
        Ok((next, ()))
    }
}

#[phoxal::runtime::outputs]
#[allow(dead_code)]
impl Counter {
    #[phoxal::runtime::outputs::state(
        port = COUNTER_STATUS,
        max_bytes = 64,
        bootstrap,
        on_change,
    )]
    fn status(&self, state: &u64) -> CounterStatus {
        CounterStatus {
            inspected_count: *state,
        }
    }

    fn read_view(&self, state: &u64) -> CounterStatus {
        self.status(state)
    }

    #[phoxal::runtime::outputs::read(
        port = COUNTER_READ,
        project = Self::read_view,
        max_request_bytes = 64,
        max_response_bytes = 64,
    )]
    fn inspect(&self, view: &CounterStatus, request: &CounterReadRequest) -> CounterReadResponse {
        CounterReadResponse {
            inspected_count: if request.include_count {
                view.inspected_count
            } else {
                0
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CounterReadKey(u64);

#[derive(Default)]
struct PeriodicReaderState {
    last_count: Option<u64>,
    last_attempt_failed: bool,
}

struct PeriodicReader;

#[phoxal::runtime::inputs]
struct PeriodicReaderInputs {
    #[phoxal::runtime::input(max_response_bytes = 64)]
    counter: Read<CounterReadKey, CounterReadRequest, CounterReadResponse>,
}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1000)]
impl Runtime for PeriodicReader {
    type Config = ();
    type State = PeriodicReaderState;
    type Inputs = PeriodicReaderInputs;
    type Outputs = ();

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(PeriodicReaderState::default())
    }

    fn step(
        &self,
        _ctx: &StepContext,
        mut state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        if let Some(completion) = inputs.counter.new_completion()
            && completion.key() == &CounterReadKey(0)
        {
            match completion.result() {
                Ok(response) => {
                    state.last_count = Some(response.inspected_count);
                    state.last_attempt_failed = false;
                }
                Err(_) => state.last_attempt_failed = true,
            }
        }
        Ok((state, ()))
    }
}

#[phoxal::runtime::outputs]
#[allow(dead_code)]
impl PeriodicReader {
    #[phoxal::runtime::outputs::activate(counter, timeout_ms = 500, refresh_every_steps = 5)]
    fn request(
        &self,
        _state: &PeriodicReaderState,
    ) -> Option<Activation<CounterReadKey, CounterReadRequest>> {
        Some(Activation::new(
            CounterReadKey(0),
            CounterReadRequest {
                include_count: true,
            },
        ))
    }

    #[phoxal::runtime::outputs::state(
        port = READER_STATUS,
        max_bytes = 64,
        bootstrap,
        on_change,
    )]
    fn status(&self, state: &PeriodicReaderState) -> ReaderStatus {
        ReaderStatus {
            last_count: state.last_count,
            finished: false,
            last_attempt_failed: state.last_attempt_failed,
        }
    }
}

enum OnceReaderState {
    Waiting,
    Finished { count: Option<u64>, failed: bool },
}

struct OnceReader;

#[phoxal::runtime::inputs]
struct OnceReaderInputs {
    #[phoxal::runtime::input(max_response_bytes = 64)]
    counter: Read<CounterReadKey, CounterReadRequest, CounterReadResponse>,
}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1000)]
impl Runtime for OnceReader {
    type Config = ();
    type State = OnceReaderState;
    type Inputs = OnceReaderInputs;
    type Outputs = ();

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(OnceReaderState::Waiting)
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        if let OnceReaderState::Waiting = state
            && let Some(completion) = inputs.counter.new_completion()
            && completion.key() == &CounterReadKey(0)
        {
            let (count, failed) = match completion.result() {
                Ok(response) => (Some(response.inspected_count), false),
                Err(_) => (None, true),
            };
            return Ok((OnceReaderState::Finished { count, failed }, ()));
        }
        Ok((state, ()))
    }
}

#[phoxal::runtime::outputs]
#[allow(dead_code)]
impl OnceReader {
    #[phoxal::runtime::outputs::activate(counter, timeout_ms = 500)]
    fn request(
        &self,
        state: &OnceReaderState,
    ) -> Option<Activation<CounterReadKey, CounterReadRequest>> {
        match state {
            OnceReaderState::Waiting => Some(Activation::new(
                CounterReadKey(0),
                CounterReadRequest {
                    include_count: true,
                },
            )),
            OnceReaderState::Finished { .. } => None,
        }
    }

    #[phoxal::runtime::outputs::state(
        port = READER_STATUS,
        max_bytes = 64,
        bootstrap,
        on_change,
    )]
    fn status(&self, state: &OnceReaderState) -> ReaderStatus {
        match state {
            OnceReaderState::Waiting => ReaderStatus {
                last_count: None,
                finished: false,
                last_attempt_failed: false,
            },
            OnceReaderState::Finished { count, failed } => ReaderStatus {
                last_count: *count,
                finished: true,
                last_attempt_failed: *failed,
            },
        }
    }
}

const VALUES: [f64; 10] = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
const SUM_STATUS: phoxal::__private::State<SumStatus> = phoxal::__private::State::new("sum-status");

#[derive(Clone, Copy, PartialEq, prost::Message)]
struct SumStatus {
    #[prost(uint32, tag = "1")]
    processed: u32,
    #[prost(double, tag = "2")]
    total: f64,
    #[prost(bool, tag = "3")]
    complete: bool,
}

impl prost::Name for SumStatus {
    const NAME: &'static str = "SumStatus";
    const PACKAGE: &'static str = "phoxal.tests.runtime";
}

struct IncrementalSum;

struct SumState {
    cursor: usize,
    total: f64,
}

impl SumState {
    fn advance(&mut self, max_values: usize) {
        let end = self.cursor.saturating_add(max_values).min(VALUES.len());
        for value in &VALUES[self.cursor..end] {
            self.total += value;
        }
        self.cursor = end;
    }
}

#[phoxal::runtime::inputs]
struct SumInputs {}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1000)]
impl Runtime for IncrementalSum {
    type Config = ();
    type State = SumState;
    type Inputs = SumInputs;
    type Outputs = ();

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(SumState {
            cursor: 0,
            total: 0.0,
        })
    }

    fn step(
        &self,
        _ctx: &StepContext,
        mut state: Self::State,
        _inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        state.advance(2);
        Ok((state, ()))
    }
}

#[phoxal::runtime::outputs]
#[allow(dead_code)]
impl IncrementalSum {
    #[phoxal::runtime::outputs::state(port = SUM_STATUS, max_bytes = 64, bootstrap, on_change)]
    fn status(&self, state: &SumState) -> SumStatus {
        SumStatus {
            processed: state.cursor as u32,
            total: state.total,
            complete: state.cursor == VALUES.len(),
        }
    }
}

fn context(index: u64, now_ms: u64) -> StepContext {
    StepContext::new(
        ExecutionTime::from_nanos(now_ms * 1_000_000),
        ExecutionDuration::from_millis(20),
        ExecutionDuration::from_millis(20),
        0,
        index,
    )
}

#[test]
fn counter_and_read_fixture_keep_projection_and_read_pure() {
    let state = initialize(&Counter, ExecutionTime::from_nanos(0), ()).expect("counter init");
    let (state, ()) = invoke(
        &Counter,
        &context(0, 20),
        state,
        &CounterInputs {
            increments: Events::new(vec![2, 3]),
        },
    )
    .expect("counter step");
    assert_eq!(Counter.status(&state), CounterStatus { inspected_count: 5 });
    assert_eq!(
        Counter.inspect(
            &Counter.read_view(&state),
            &CounterReadRequest {
                include_count: true,
            },
        ),
        CounterReadResponse { inspected_count: 5 }
    );
}

#[test]
fn periodic_reader_retains_success_during_refresh_and_reports_failure() {
    let reader = PeriodicReader;
    let state = initialize(&reader, ExecutionTime::from_nanos(0), ()).expect("reader init");
    let (state, ()) = invoke(
        &reader,
        &context(0, 20),
        state,
        &PeriodicReaderInputs {
            counter: Read::completed(
                CounterReadKey(0),
                Ok(CounterReadResponse { inspected_count: 9 }),
            ),
        },
    )
    .expect("successful read");
    assert_eq!(state.last_count, Some(9));
    assert!(!state.last_attempt_failed);

    let retained = phoxal::runtime::input::ReadSuccess::new(
        CounterReadKey(0),
        CounterReadResponse { inspected_count: 9 },
        ObservationStamp::new("counter", ExecutionTime::from_nanos(20_000_000), Some(1)),
    );
    let (state, ()) = invoke(
        &reader,
        &context(1, 40),
        state,
        &PeriodicReaderInputs {
            counter: Read::pending(CounterReadKey(0)).with_retained_success(retained),
        },
    )
    .expect("refresh pending");
    assert_eq!(state.last_count, Some(9));
    assert!(!state.last_attempt_failed);

    let (state, ()) = invoke(
        &reader,
        &context(2, 60),
        state,
        &PeriodicReaderInputs {
            counter: Read::completed(CounterReadKey(0), Err(ReadError::Timeout)),
        },
    )
    .expect("failed refresh");
    assert_eq!(state.last_count, Some(9));
    assert!(state.last_attempt_failed);
}

#[test]
fn once_reader_finishes_on_failure_without_implicit_retry() {
    let reader = OnceReader;
    let state = initialize(&reader, ExecutionTime::from_nanos(0), ()).expect("reader init");
    let (state, ()) = invoke(
        &reader,
        &context(0, 20),
        state,
        &OnceReaderInputs {
            counter: Read::completed(
                CounterReadKey(0),
                Err(ReadError::Unavailable("counter stopped".into())),
            ),
        },
    )
    .expect("terminal failure");
    assert!(matches!(
        state,
        OnceReaderState::Finished {
            count: None,
            failed: true
        }
    ));
}

#[test]
fn incremental_sum_advances_a_bounded_persistent_cursor() {
    let sum = IncrementalSum;
    let mut state = initialize(&sum, ExecutionTime::from_nanos(0), ()).expect("sum init");
    let inputs = SumInputs {};
    for index in 0..5 {
        let (next, ()) = invoke(&sum, &context(index, 20 * (index + 1)), state, &inputs)
            .expect("bounded sum step");
        state = next;
    }
    assert_eq!(state.cursor, VALUES.len());
    assert_eq!(state.total, 55.0);
    assert_eq!(
        sum.status(&state),
        SumStatus {
            processed: 10,
            total: 55.0,
            complete: true,
        }
    );
    let (state, ()) = invoke(&sum, &context(5, 120), state, &inputs).expect("no-op after finish");
    assert_eq!(state.cursor, VALUES.len());
    assert_eq!(state.total, 55.0);
}

#[test]
fn read_status_distinguishes_pending_and_completed_inputs() {
    assert_eq!(
        Read::<CounterReadKey, CounterReadRequest, CounterReadResponse>::pending(CounterReadKey(0))
            .status(),
        ReadStatus::Pending
    );
    assert_eq!(
        Read::<CounterReadKey, CounterReadRequest, CounterReadResponse>::completed(
            CounterReadKey(0),
            Ok(CounterReadResponse { inspected_count: 1 }),
        )
        .status(),
        ReadStatus::Completed
    );
}

#[test]
fn compiled_input_records_retain_concrete_owner_message_names() {
    let bytes = __PHOXAL_RUNTIME_ARTIFACT_periodic_reader.as_bytes();
    let record: RuntimeRecord = serde_json::from_slice(&bytes[12..]).unwrap();
    let RuntimeRecord::V0 { inputs, .. } = &record;
    let input = &inputs[0];
    assert_eq!(
        input.request_fqn.as_deref(),
        Some("phoxal.tests.runtime.CounterReadRequest")
    );
    assert_eq!(
        input.response_fqn.as_deref(),
        Some("phoxal.tests.runtime.CounterReadResponse")
    );
    let counter: RuntimeRecord =
        serde_json::from_slice(&__PHOXAL_RUNTIME_ARTIFACT_counter.as_bytes()[12..]).unwrap();
    let RuntimeRecord::V0 { inputs, .. } = &counter;
    assert_eq!(
        inputs[0].response_fqn.as_deref(),
        Some("google.protobuf.UInt64Value")
    );
}
