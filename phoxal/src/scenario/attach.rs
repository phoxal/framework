//! P5 typed application-attachment surface.
//!
//! Production code attaches a scenario bundle to a runtime through
//! the existing `Runtime::attach_input` / `attach_output` machinery.
//! This module adds a typed wrapper that records the attachment
//! alongside the program identity so the supervisor can verify the
//! attached input edge matches the substituted producer declared on
//! the scenario bundle.
//!
//! The runtime crate owns the real attach primitives; this module
//! is a typed sugar layer that exists so the case host can quote
//! the attachment in one call without dipping into the runner types.

use phoxal_port::PortSignature;

/// One typed application attachment. Captures the runtime instance
/// identifier and the typed port descriptor the application binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationAttachment {
    pub runtime_instance: String,
    pub producer_signature: PortSignature,
    pub scenario_name: String,
}

impl ApplicationAttachment {
    /// Construct an attachment record. The case host calls this
    /// once per scenario-driven producer substitution.
    pub fn new(
        runtime_instance: impl Into<String>,
        producer_signature: PortSignature,
        scenario_name: impl Into<String>,
    ) -> Self {
        Self {
            runtime_instance: runtime_instance.into(),
            producer_signature,
            scenario_name: scenario_name.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal_port::PortKind;

    fn sig() -> PortSignature {
        PortSignature::new(
            "motion/state",
            "phoxal.motion",
            "State",
            PortKind::State,
            "State",
            "State",
        )
    }

    #[test]
    fn carries_runtime_and_scenario_identity() {
        let attachment = ApplicationAttachment::new("instance-1", sig(), "scenarios/First");
        assert_eq!(attachment.runtime_instance, "instance-1");
        assert_eq!(attachment.scenario_name, "scenarios/First");
        assert_eq!(attachment.producer_signature.name, "motion/state");
    }
}