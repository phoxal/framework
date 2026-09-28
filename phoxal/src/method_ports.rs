//! Runtime provider adapters for the inert contract method descriptors.
//!
//! The descriptors in [`crate::contracts`] stay inert in every profile;
//! these inherent methods adapt them to the provider port collectors the
//! runtime uses. Compiling this module is selected once, at the crate
//! root, by the `runtime` feature.

use crate::contracts::{CallMethod, ObservationMethod};

impl<Request, Response> CallMethod<Request, Response> {
    /// Adapts a non-leased call to the existing provider ingress collector.
    #[must_use]
    pub const fn commands_port(self) -> crate::port::Commands<Request, Response> {
        let signature = self.signature();
        assert!(
            signature.lease.is_none(),
            "leased calls use the setpoint provider adapter"
        );
        crate::port::Commands::from_signature(crate::port::PortSignature::from_method(
            signature,
            crate::port::PortKind::Commands,
        ))
    }

    /// Adapts a leased call to the existing provider ingress collector.
    #[must_use]
    pub const fn setpoint_port(self) -> crate::port::Setpoint<Request> {
        let signature = self.signature();
        assert!(
            signature.lease.is_some(),
            "only leased calls use the setpoint provider adapter"
        );
        crate::port::Setpoint::from_signature(crate::port::PortSignature::from_method(
            signature,
            crate::port::PortKind::Setpoint,
        ))
    }
}

impl<Value> ObservationMethod<Value> {
    /// Adapts a retained observation to the existing provider projection collector.
    #[must_use]
    pub const fn state_port(self) -> crate::port::State<Value> {
        let signature = self.signature();
        assert!(
            signature.retained_latest,
            "only retained observations use the state provider adapter"
        );
        crate::port::State::from_signature(crate::port::PortSignature::from_method(
            signature,
            crate::port::PortKind::State,
        ))
    }

    /// Adapts a non-retained observation to the existing provider sample collector.
    #[must_use]
    pub const fn sample_port(self) -> crate::port::Sample<Value> {
        let signature = self.signature();
        assert!(
            !signature.retained_latest,
            "retained observations use the state provider adapter"
        );
        crate::port::Sample::from_signature(crate::port::PortSignature::from_method(
            signature,
            crate::port::PortKind::Sample,
        ))
    }

    /// Adapts a non-retained observation to an event provider collector.
    #[must_use]
    pub const fn event_port(self) -> crate::port::Event<Value> {
        let signature = self.signature();
        assert!(
            !signature.retained_latest,
            "retained observations use the state provider adapter"
        );
        crate::port::Event::from_signature(crate::port::PortSignature::from_method(
            signature,
            crate::port::PortKind::Event,
        ))
    }

    /// Adapts a non-retained observation to a stream provider collector.
    #[must_use]
    pub const fn stream_port(self) -> crate::port::Stream<Value> {
        let signature = self.signature();
        assert!(
            !signature.retained_latest,
            "retained observations use the state provider adapter"
        );
        crate::port::Stream::from_signature(crate::port::PortSignature::from_method(
            signature,
            crate::port::PortKind::Stream,
        ))
    }

    /// Adapts a leased observation to the existing provider setpoint collector.
    #[must_use]
    pub const fn setpoint_port(self) -> crate::port::Setpoint<Value> {
        let signature = self.signature();
        assert!(
            signature.lease.is_some(),
            "only leased observations use the setpoint provider adapter"
        );
        crate::port::Setpoint::from_signature(crate::port::PortSignature::from_method(
            signature,
            crate::port::PortKind::Setpoint,
        ))
    }
}
