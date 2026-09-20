//! Scenario-only bundle admission policy owned by the supervisor binary.

/// Stable marker carried by controlled scenario bundles.
pub(super) const SCENARIO_NONDEPLOYABLE: &str = "phoxal/scenario/nondeployable@1";

/// Launch mode requested for a bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScenarioLaunchMode {
    Controlled,
    Hardware,
}

/// Admission verdict returned by [`evaluate_scenario_admission`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ScenarioAdmission {
    Accept {
        scenario_name: String,
        digest: String,
    },
    RefuseNondeployableOnHardware,
    RefuseMissingIdentity,
    RefuseMalformedDigest(String),
}

/// Evaluate the pure admission policy before the runtime starts a bundle.
pub(super) fn evaluate_scenario_admission(
    mode: ScenarioLaunchMode,
    scenario_name: &str,
    program_byte_length: u32,
    program_digest: &str,
    bundle_marker: Option<&str>,
) -> ScenarioAdmission {
    if bundle_marker == Some(SCENARIO_NONDEPLOYABLE) && matches!(mode, ScenarioLaunchMode::Hardware)
    {
        return ScenarioAdmission::RefuseNondeployableOnHardware;
    }
    if bundle_marker == Some(SCENARIO_NONDEPLOYABLE) && program_byte_length == 0 {
        return ScenarioAdmission::RefuseMissingIdentity;
    }
    if bundle_marker == Some(SCENARIO_NONDEPLOYABLE)
        && (program_digest.len() != 64
            || !program_digest
                .chars()
                .all(|character| character.is_ascii_hexdigit()))
    {
        return ScenarioAdmission::RefuseMalformedDigest(program_digest.to_owned());
    }
    ScenarioAdmission::Accept {
        scenario_name: scenario_name.to_owned(),
        digest: program_digest.to_owned(),
    }
}

/// Render a stable refusal diagnostic, or `None` for an accepted bundle.
pub(super) fn admission_diagnostic(verdict: &ScenarioAdmission) -> Option<String> {
    match verdict {
        ScenarioAdmission::Accept { .. } => None,
        ScenarioAdmission::RefuseNondeployableOnHardware => Some(format!(
            "scenario bundle carries `{SCENARIO_NONDEPLOYABLE}`; hardware launch refuses scenario bundles outright"
        )),
        ScenarioAdmission::RefuseMissingIdentity => Some(
            "scenario bundle has no program identity; byte_length and program_digest are required"
                .to_owned(),
        ),
        ScenarioAdmission::RefuseMalformedDigest(digest) => Some(format!(
            "scenario bundle digest `{digest}` is not 64 lowercase hex characters"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
