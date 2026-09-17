//! Public protocol types for the supervisor executable package.
//!
//! The supervisor is a single-purpose binary that admits one
//! immutable source bundle, starts the embedded router, launches its
//! recorded Runtime processes, and serves the public session on the
//! same execution transport. This library exposes the protocol
//! types and the scenario admission policy so other binaries
//! (notably `phoxal`) can speak the same language without dragging
//! in the supervisor's runtime.
#![allow(unused_imports, dead_code)]

pub use phoxal::communication as communication;
pub use phoxal::communication::{DeploymentTarget, session, simulation};
pub use phoxal::identity as identity;

// P2.6 scenario admission policy. Lifted into the supervisor's lib
// crate so the integration tests and any future case host binary can
// quote the same nondeployable marker without depending on the
// supervisor's binary entry point.
pub mod scenario_admission {
    //! Stable marker carried by controlled scenario bundles. The bundle
    //! producer and the supervisor admission logic must use the same
    //! value.
    pub const SCENARIO_NONDEPLOYABLE: &str = "phoxal/scenario/nondeployable@1";

    /// Launch mode the case host asks the supervisor to admit a bundle
    /// into. We accept the enum here so this module stays free of any
    /// dependency on the supervisor's runtime types.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ScenarioLaunchMode {
        Controlled,
        Hardware,
    }

    /// Admission verdict returned by `evaluate_scenario_admission`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ScenarioAdmission {
        Accept {
            scenario_name: String,
            digest: String,
        },
        RefuseNondeployableOnHardware,
        RefuseMissingIdentity,
        RefuseMalformedDigest(String),
    }

    /// Pure admission policy check used by both the runtime admission
    /// path and the installation validation.
    pub fn evaluate_scenario_admission(
        mode: ScenarioLaunchMode,
        scenario_name: &str,
        program_byte_length: u32,
        program_digest: &str,
        bundle_marker: Option<&str>,
    ) -> ScenarioAdmission {
        if bundle_marker == Some(SCENARIO_NONDEPLOYABLE)
            && matches!(mode, ScenarioLaunchMode::Hardware)
        {
            return ScenarioAdmission::RefuseNondeployableOnHardware;
        }
        if bundle_marker == Some(SCENARIO_NONDEPLOYABLE) && program_byte_length == 0 {
            return ScenarioAdmission::RefuseMissingIdentity;
        }
        if bundle_marker == Some(SCENARIO_NONDEPLOYABLE)
            && (program_digest.len() != 64
                || !program_digest.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return ScenarioAdmission::RefuseMalformedDigest(program_digest.to_owned());
        }
        ScenarioAdmission::Accept {
            scenario_name: scenario_name.to_owned(),
            digest: program_digest.to_owned(),
        }
    }

    /// Quote the admission decision into a stable diagnostic string
    /// the supervisor logs at admission time. Returns `None` for the
    /// `Accept` verdict because the caller does not need a log line.
    pub fn admission_diagnostic(verdict: &ScenarioAdmission) -> Option<String> {
        match verdict {
            ScenarioAdmission::Accept { .. } => None,
            ScenarioAdmission::RefuseNondeployableOnHardware => Some(format!(
                "scenario bundle carries `{SCENARIO_NONDEPLOYABLE}`; hardware launch refuses \
                 scenario bundles outright"
            )),
            ScenarioAdmission::RefuseMissingIdentity => Some(
                "scenario bundle has no program identity; byte_length and program_digest are \
                 required"
                    .to_owned(),
            ),
            ScenarioAdmission::RefuseMalformedDigest(digest) => Some(format!(
                "scenario bundle digest `{digest}` is not 64 lowercase hex characters"
            )),
        }
    }
}

pub use scenario_admission::{
    SCENARIO_NONDEPLOYABLE, ScenarioAdmission, ScenarioLaunchMode, admission_diagnostic,
    evaluate_scenario_admission,
};

#[cfg(test)]
mod admission_tests {
    use super::scenario_admission::*;

    #[test]
    fn nondeployable_marker_is_stable() {
        assert_eq!(SCENARIO_NONDEPLOYABLE, "phoxal/scenario/nondeployable@1");
    }

    #[test]
    fn hardware_launch_refuses_nondeployable() {
        let verdict = evaluate_scenario_admission(
            ScenarioLaunchMode::Hardware,
            "scenarios/First",
            16,
            &"a".repeat(64),
            Some(SCENARIO_NONDEPLOYABLE),
        );
        assert_eq!(verdict, ScenarioAdmission::RefuseNondeployableOnHardware);
        assert!(admission_diagnostic(&verdict).is_some());
    }

    #[test]
    fn controlled_launch_admits_nondeployable() {
        let verdict = evaluate_scenario_admission(
            ScenarioLaunchMode::Controlled,
            "scenarios/First",
            16,
            &"a".repeat(64),
            Some(SCENARIO_NONDEPLOYABLE),
        );
        match verdict {
            ScenarioAdmission::Accept {
                scenario_name,
                digest,
            } => {
                assert_eq!(scenario_name, "scenarios/First");
                assert_eq!(digest.len(), 64);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn missing_identity_refuses() {
        let verdict = evaluate_scenario_admission(
            ScenarioLaunchMode::Controlled,
            "scenarios/First",
            0,
            &"a".repeat(64),
            Some(SCENARIO_NONDEPLOYABLE),
        );
        assert_eq!(verdict, ScenarioAdmission::RefuseMissingIdentity);
    }

    #[test]
    fn malformed_digest_refuses() {
        let verdict = evaluate_scenario_admission(
            ScenarioLaunchMode::Controlled,
            "scenarios/First",
            16,
            "not-hex",
            Some(SCENARIO_NONDEPLOYABLE),
        );
        assert!(matches!(
            verdict,
            ScenarioAdmission::RefuseMalformedDigest(_)
        ));
    }

    #[test]
    fn ordinary_bundles_pass_unmodified() {
        let verdict = evaluate_scenario_admission(
            ScenarioLaunchMode::Hardware,
            "scenarios/Other",
            0,
            "",
            None,
        );
        assert!(matches!(verdict, ScenarioAdmission::Accept { .. }));
    }
}
