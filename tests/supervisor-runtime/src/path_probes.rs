//! Ordinary Rust path spellings for contract attachment.
//!
//! Each probe uses the spelling a normal author would write: a payload
//! referenced through `super::`, a contract referenced by its plain module
//! path, and a contract referenced through a renamed import.  All three
//! must attach through the contract type itself, never a derived module or
//! macro path.

use phoxal::runtime::Context;

/// A vocabulary module one level deeper than the contract that uses it.
pub mod vocabulary {
    /// Referenced from the sibling contract module through `super::`.
    #[phoxal::message(package = "example.path_probe.v1")]
    pub struct ProbeState {
        #[phoxal(tag = 1)]
        pub value: u64,
    }

    /// A nested module whose message references `super::ProbeState` from one
    /// level deeper.
    pub mod detail {
        #[phoxal::message(package = "example.path_probe.v1")]
        pub struct ProbeSummary {
            #[phoxal(tag = 1)]
            pub state: Option<super::ProbeState>,
        }
    }

    pub use detail::ProbeSummary;
}

/// A contract module separate from the runtimes that attach to it.
pub mod contracts {
    use super::vocabulary::{ProbeState, ProbeSummary};
    use phoxal::contracts::Latest;

    /// Attached by its plain relative path from another module.
    #[phoxal::endpoints]
    pub struct LocalApi {
        #[phoxal::output(projection = state, max_bytes = 512)]
        state: Latest<ProbeState>,

        #[phoxal::output(projection = state, max_bytes = 512)]
        summary: Latest<ProbeSummary>,
    }

    /// Attached through a renamed import.
    #[phoxal::endpoints]
    pub struct LessorApi {
        #[phoxal::output(projection = state, lease_ms = 500, max_bytes = 512)]
        leased: Latest<ProbeState>,
    }
}

use contracts::LessorApi as ImportedLessorApi;

/// Attaches `LocalApi` through its plain relative module path.
pub struct RelativeRuntime {
    value: u64,
}

#[phoxal::runtime(contract = contracts::LocalApi, period_ms = 20)]
impl RelativeRuntime {
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {
        Ok(Self { value: 0 })
    }

    #[step]
    fn advance(&mut self, _ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
        self.value = self.value.saturating_add(1);
        Ok(())
    }

    #[publish(state)]
    fn state(&self) -> vocabulary::ProbeState {
        vocabulary::ProbeState { value: self.value }
    }

    #[publish(summary)]
    fn summary(&self) -> vocabulary::ProbeSummary {
        vocabulary::ProbeSummary {
            state: Some(vocabulary::ProbeState { value: self.value }),
        }
    }
}

/// Attaches a leased projection contract through a renamed import.
pub struct ImportedRuntime {
    value: u64,
}

#[phoxal::runtime(contract = ImportedLessorApi, period_ms = 20)]
impl ImportedRuntime {
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {
        Ok(Self { value: 0 })
    }

    #[step]
    fn advance(&mut self, _ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
        self.value = self.value.saturating_add(1);
        Ok(())
    }

    #[publish(leased)]
    fn leased(&self) -> Option<vocabulary::ProbeState> {
        (self.value > 0).then_some(vocabulary::ProbeState { value: self.value })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::runtime::StepContext;
    use phoxal::runtime::outputs::OutputBindings;

    #[test]
    fn relative_and_imported_attachments_bind_their_own_endpoints() {
        let fields = <super::phoxal_runtime_relative_runtime::Adapter as OutputBindings>::FIELDS;
        assert_eq!(
            fields.iter().map(|field| field.name).collect::<Vec<_>>(),
            vec!["state", "summary"],
            "relative attachment binds its own endpoints"
        );
        let leased = <super::phoxal_runtime_imported_runtime::Adapter as OutputBindings>::FIELDS;
        assert_eq!(
            leased.iter().map(|field| field.name).collect::<Vec<_>>(),
            vec!["leased"],
            "renamed-import attachment binds its own endpoints"
        );
        assert!(
            leased[0].valid_for_ms.is_some(),
            "the leased projection keeps its lease bound"
        );
        assert!(
            fields[0].port == Some("state") && leased[0].port == Some("leased"),
            "each attachment names its own endpoints, found {:?} and {:?}",
            fields[0].port,
            leased[0].port
        );
        assert!(
            fields[0]
                .port_signature
                .is_some_and(|signature| signature.response.contains("ProbeState")),
            "the bound port carries its payload identity, found {:?}",
            fields[0].port_signature.map(|signature| signature.response)
        );
    }

    #[test]
    fn attachments_encode_projections_through_the_contract_type() {
        let context = StepContext::first(
            phoxal::runtime::ExecutionTime::from_nanos(20_000_000),
            phoxal::runtime::ExecutionDuration::from_millis(20),
        );
        let relative = super::phoxal_runtime_relative_runtime::Adapter::new()
            .encode_transport(&RelativeRuntime { value: 7 }, context, &|_| None, "probe")
            .expect("relative projections encode");
        assert_eq!(relative.len(), 2, "one prepared output per projection");

        let imported = super::phoxal_runtime_imported_runtime::Adapter::new()
            .encode_transport(&ImportedRuntime { value: 7 }, context, &|_| None, "probe")
            .expect("imported projections encode");
        assert_eq!(imported.len(), 1, "the leased projection encodes");
    }
}
