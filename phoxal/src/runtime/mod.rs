//! What a running Phoxal process says about itself.
//!
//! [`api`] is the `runtime` contract family: log events plus bus and step
//! telemetry. Any process publishes here; the family names no collector.

mod core;
pub mod input;
mod operation;
pub mod outputs;
mod schedule;

// The collector attributes live in the runtime namespace so authors can use
// the exact `#[phoxal::runtime::inputs]` and `#[phoxal::runtime::input(...)]`
// spellings without importing a second macro crate.
#[doc(hidden)]
pub use phoxal_macros::outputs;
#[doc(hidden)]
pub use phoxal_macros::{input, inputs};

pub use core::{
    AcceptedInvocation, Config, ExecutionDuration, ExecutionTime, InitContext, Invocation,
    InvocationError, ObservationStamp, OutputAdmission, RegisteredRuntime, Runtime, RuntimeOwner,
    RuntimeSpec, RuntimeSpecError, RuntimeStatus, Sample, StepContext, initialize, invoke, run,
};
pub use operation::{
    ManagedOperation, OperationCompletion, OperationError, OperationKey, OperationOutcome,
    OperationPolicy, OperationState,
};
pub use schedule::{HardwareInvocation, HardwareSchedule, ScheduleError};

pub use input::{
    Activation, Capacity, CapacityError, Command, CommandId, CommandOrder, CommandOrderError,
    Commands, Events, Latest, Operation, OperationInputError, Read, ReadCompletion, ReadError,
    ReadStatus, ReadSuccess, Reply, Request, RequestCompletion, RequestError, Samples, Setpoint,
    Stream, StreamFailure, StreamItem,
};

/// The common descriptor checks emitted by the runtime authoring macros.
///
/// These traits are intentionally small.  A generated `phoxal-port` descriptor
/// is still the source of truth for its endpoint identity; the traits only make
/// the descriptor's typed payload available to compile-time checks in a
/// downstream service crate.
#[doc(hidden)]
pub mod __private {
    pub use crate::port::{PortDescriptor, PortKind};

    /// A state projection has a payload matching the served state descriptor.
    pub trait StatePortValue<Value>: PortDescriptor {}

    /// A sample publication has a payload matching the served sample
    /// descriptor.
    pub trait SamplePortValue<Value>: PortDescriptor {}

    /// An event publication has a payload matching the served event descriptor.
    pub trait EventPortValue<Value>: PortDescriptor {}

    /// A stream publication has a payload matching the served stream
    /// descriptor.
    pub trait StreamPortValue<Value>: PortDescriptor {}

    /// A setpoint projection has a payload matching the served setpoint
    /// descriptor.
    pub trait SetpointPortValue<Value>: PortDescriptor {}

    /// A read handler has matching request and response payloads.
    pub trait ReadPortValue<Request, Response>: PortDescriptor {}

    /// A commands binding has matching request and response payloads.
    pub trait CommandsPortValue<Request, Response>: PortDescriptor {}

    impl<T: 'static> StatePortValue<T> for crate::port::State<T> {}
    impl<T: 'static> StatePortValue<crate::runtime::Sample<T>> for crate::port::State<T> {}
    impl<T: 'static> SamplePortValue<T> for crate::port::Sample<T> {}
    impl<T: 'static> EventPortValue<T> for crate::port::Event<T> {}
    impl<T: 'static> StreamPortValue<T> for crate::port::Stream<T> {}
    impl<T: 'static> SetpointPortValue<T> for crate::port::Setpoint<T> {}
    impl<T: 'static> SetpointPortValue<Option<&T>> for crate::port::Setpoint<T> {}
    impl<T: 'static> SetpointPortValue<Option<T>> for crate::port::Setpoint<T> {}
    impl<Request: 'static, Response: 'static> ReadPortValue<Request, Response>
        for crate::port::Read<Request, Response>
    {
    }
    impl<Request: 'static, Response: 'static> CommandsPortValue<Request, Response>
        for crate::port::Commands<Request, Response>
    {
    }

    /// Checks a state descriptor and its method value type.
    pub fn assert_state_port<P, Value>(port: P)
    where
        P: StatePortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::State);
    }

    /// Checks a sample descriptor and its field value type.
    pub fn assert_sample_port<P, Value>(port: P)
    where
        P: SamplePortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Sample);
    }

    /// Checks an event descriptor and its field value type.
    pub fn assert_event_port<P, Value>(port: P)
    where
        P: EventPortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Event);
    }

    /// Checks a stream descriptor and its field value type.
    pub fn assert_stream_port<P, Value>(port: P)
    where
        P: StreamPortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Stream);
    }

    /// Checks a setpoint descriptor and its method value type.
    pub fn assert_setpoint_port<P, Value>(port: P)
    where
        P: SetpointPortValue<Value>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Setpoint);
    }

    /// Checks a read descriptor and its handler payloads.
    pub fn assert_read_port<P, Request, Response>(port: P)
    where
        P: ReadPortValue<Request, Response>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Read);
    }

    /// Checks a commands descriptor and its input payloads.
    pub fn assert_commands_port<P, Request, Response>(port: P)
    where
        P: CommandsPortValue<Request, Response>,
    {
        let _ = port;
        assert_kind::<P>(PortKind::Commands);
    }

    fn assert_kind<P: PortDescriptor>(expected: PortKind) {
        assert!(
            P::KIND == expected,
            "port descriptor kind does not match its runtime role"
        );
    }
}

/// The `runtime` contract family.
pub mod api;
