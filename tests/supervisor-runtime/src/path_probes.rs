//! Ordinary Rust path spellings for contract attachment.
//!
//! Each probe uses the spelling a normal author would write: a payload
//! referenced through `super::`, a contract referenced by its plain module
//! path, and a contract referenced through a renamed import.  All three
//! must attach through the contract type itself, never a derived module or
//! macro path.

use phoxal::runtime::{InitContext, Runtime, StepContext};

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
pub struct RelativeRuntime;

#[phoxal::runtime(contract = contracts::LocalApi, period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for RelativeRuntime {
    type Config = ();
    type State = u64;

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(0)
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        _inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        Ok((state.saturating_add(1), Self::Outputs::default()))
    }
}

impl contracts::local_api::projections::Projections for RelativeRuntime {
    type State = u64;

    fn state(&self, state: &u64) -> vocabulary::ProbeState {
        vocabulary::ProbeState { value: *state }
    }

    fn summary(&self, state: &u64) -> vocabulary::ProbeSummary {
        vocabulary::ProbeSummary {
            state: Some(vocabulary::ProbeState { value: *state }),
        }
    }
}

/// Attaches a leased projection contract through a renamed import.
pub struct ImportedRuntime;

#[phoxal::runtime(contract = ImportedLessorApi, period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for ImportedRuntime {
    type Config = ();
    type State = u64;

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(0)
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        _inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        Ok((state.saturating_add(1), Self::Outputs::default()))
    }
}

impl contracts::lessor_api::projections::Projections for ImportedRuntime {
    type State = u64;

    fn leased(&self, state: &u64) -> Option<vocabulary::ProbeState> {
        (*state > 0).then_some(vocabulary::ProbeState { value: *state })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::runtime::outputs::OutputBindings;

    #[test]
    fn relative_and_imported_attachments_bind_their_own_endpoints() {
        let fields = <RelativeRuntime as OutputBindings>::FIELDS;
        assert_eq!(
            fields.iter().map(|field| field.name).collect::<Vec<_>>(),
            vec!["state", "summary"],
            "relative attachment binds its own endpoints"
        );
        let leased = <ImportedRuntime as OutputBindings>::FIELDS;
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
        let relative = RelativeRuntime
            .encode_transport(&7_u64, context, &|_| None, "probe")
            .expect("relative projections encode");
        assert_eq!(relative.len(), 2, "one prepared output per projection");

        let imported = ImportedRuntime
            .encode_transport(&7_u64, context, &|_| None, "probe")
            .expect("imported projections encode");
        assert_eq!(imported.len(), 1, "the leased projection encodes");
    }
}
