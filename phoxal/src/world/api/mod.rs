//! The backend-neutral `world` wire family.

crate::nodes! {
    family World;

    session;
}

/// The world family's endpoint and persisted-document compatibility surface.
#[doc(hidden)]
pub mod __compat {
    use crate::__compat::surface::{ContractRecord, ContractSurface};

    /// The canonical rendering of the complete world-owned contract surface.
    #[must_use]
    pub fn contract_surface() -> String {
        let mut records = Vec::new();
        contract_records(&mut records);
        ContractSurface::new(records).canonical_json()
    }

    /// Every endpoint and persisted document owned by the world family.
    pub(crate) fn contract_records(out: &mut Vec<ContractRecord>) {
        super::contract_records(out);
        super::session::document::__compat::contract_records(out);
    }

    #[cfg(test)]
    mod tests {
        use super::contract_surface;

        #[test]
        fn the_surface_contains_all_session_documents_and_endpoints() {
            let rendered = contract_surface();
            serde_json::from_str::<serde_json::Value>(&rendered).expect("the surface is JSON");
            assert_eq!(contract_surface(), rendered);
            for expected in [
                r#""tag":"phoxal/local-world-registration/v0""#,
                r#""tag":"phoxal/world-checkpoint/v0""#,
                r#""tag":"phoxal/world-member-terminal/v0""#,
                r#""tag":"phoxal/world-terminal-summary/v0""#,
                r#""path":"world/session/state""#,
            ] {
                assert!(
                    rendered.contains(expected),
                    "{expected} missing: {rendered}"
                );
            }
        }
    }
}
