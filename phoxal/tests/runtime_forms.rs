use phoxal::runtime::input::{
    Commands, Events, Latest, Operation, Read, Request, Samples, Setpoint, Stream,
};
use phoxal::runtime::{Activation, InitContext, Runtime, StepContext, StreamItem};

struct Forms;

type Key = u64;

const STATE: phoxal::port::State<u8> = phoxal::port::State::new("state");
const SAMPLE: phoxal::port::Sample<u16> = phoxal::port::Sample::new("sample");
const EVENT: phoxal::port::Event<u32> = phoxal::port::Event::new("event");
const STREAM: phoxal::port::Stream<u64> = phoxal::port::Stream::new("stream");
const SETPOINT: phoxal::port::Setpoint<u8> = phoxal::port::Setpoint::new("setpoint");
const READ: phoxal::port::Read<u16, u32> = phoxal::port::Read::new("read");
const COMMANDS: phoxal::port::Commands<u16, u32> = phoxal::port::Commands::new("commands");

#[phoxal::runtime::inputs]
#[allow(dead_code)]
struct FormsInputs {
    #[phoxal::runtime::input(max_age_ms = 100)]
    latest: Latest<u8>,
    #[phoxal::runtime::input(max_items = 2, max_bytes = 64)]
    samples: Samples<u16>,
    #[phoxal::runtime::input(max_items = 2, max_bytes = 64)]
    events: Events<u32>,
    setpoint: Setpoint<u8>,
    #[phoxal::runtime::input(max_items = 2, max_bytes = 64)]
    stream: Stream<u64>,
    #[phoxal::runtime::input(port = COMMANDS, max_items = 2, max_bytes = 64)]
    commands: Commands<u16, u32>,
    #[phoxal::runtime::input(max_response_bytes = 64)]
    read: Read<Key, u16, u32>,
    #[phoxal::runtime::input(max_response_bytes = 64)]
    request: Request<Key, u16, u32>,
    operation: Operation<Key, u32>,
}

#[derive(Default)]
#[phoxal::runtime::outputs]
#[allow(dead_code)]
struct FormsOutputs {
    #[phoxal::runtime::outputs::reply(commands, max_items = 2, max_bytes = 64)]
    replies: Vec<phoxal::runtime::Reply<u32>>,
    #[phoxal::runtime::outputs::sample(port = SAMPLE, max_items = 2, max_bytes = 64)]
    samples: Vec<phoxal::runtime::Sample<u16>>,
    #[phoxal::runtime::outputs::event(port = EVENT, max_items = 2, max_bytes = 64)]
    events: Vec<u32>,
    #[phoxal::runtime::outputs::stream(port = STREAM, max_items = 2, max_bytes = 64)]
    stream: Vec<StreamItem<u64>>,
}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1000)]
impl Runtime for Forms {
    type Config = ();
    type State = u8;
    type Inputs = FormsInputs;
    type Outputs = FormsOutputs;

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(0)
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        _inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        Ok((state, FormsOutputs::default()))
    }
}

#[phoxal::runtime::outputs]
#[allow(dead_code)]
impl Forms {
    #[phoxal::runtime::outputs::state(port = STATE, max_bytes = 64, bootstrap)]
    fn state(&self, state: &u8) -> u8 {
        *state
    }

    #[phoxal::runtime::outputs::setpoint(port = SETPOINT, max_bytes = 64, valid_for_ms = 100)]
    fn setpoint(&self, state: &u8) -> Option<u8> {
        Some(*state)
    }

    #[phoxal::runtime::outputs::read(
        port = READ,
        project = Self::state,
        max_request_bytes = 64,
        max_response_bytes = 64,
    )]
    fn read(&self, _view: &u8, request: &u16) -> u32 {
        u32::from(*request)
    }

    #[phoxal::runtime::outputs::activate(read, timeout_ms = 100, refresh_every_steps = 2)]
    fn read_request(&self, _state: &u8) -> Option<Activation<Key, u16>> {
        Some(Activation::new(0, 1))
    }

    #[phoxal::runtime::outputs::activate(request, timeout_ms = 100)]
    fn command_request(&self, _state: &u8) -> Option<Activation<Key, u16>> {
        Some(Activation::new(0, 1))
    }

    #[phoxal::runtime::outputs::activate(operation)]
    fn operation_request(&self, _state: &u8) -> Option<Activation<Key, u32>> {
        Some(Activation::new(0, 1))
    }

    #[phoxal::runtime::outputs::operation(operation, timeout_ms = 100, cancel_grace_ms = 20)]
    fn operation_worker(input: u32) -> phoxal::Result<u32> {
        Ok(input + 1)
    }
}

#[test]
fn all_initial_runtime_forms_compile_and_register() {
    assert_eq!(
        <Forms as phoxal::runtime::RegisteredRuntime>::SPEC
            .period
            .as_millis(),
        20
    );
    assert_eq!(
        <FormsInputs as phoxal::runtime::input::InputSet>::FIELDS[5].port,
        Some("commands")
    );
    let outputs = <Forms as phoxal::runtime::outputs::OutputBindings>::FIELDS;
    assert_eq!(outputs[2].project, Some("Self :: state"));
    assert_eq!(outputs[2].max_request_bytes, Some(64));
    assert_eq!(outputs[3].input, Some("read"));
}
