use std::collections::BTreeMap;

use crate::model::component::v1::CapabilityRef;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use super::conformance::{ConformanceEvidence, ConformanceFailure, ConformanceReport};
use super::localize_backend::{LocalizeBackendKind, resolve_localize_backend};
use super::role_resolution::resolve_roles;
use super::{AutonomyProfileId, Robot, Role, RoleResolution, autonomy_profile};

#[derive(Debug, Clone)]
pub struct SourceBundle {
    pub model: Robot,
    pub components: BTreeMap<String, crate::model::component::v1::Component>,
    pub autonomy_profile: AutonomyProfileId,
}

impl SourceBundle {
    #[must_use]
    pub fn new(
        model: Robot,
        components: BTreeMap<String, crate::model::component::v1::Component>,
    ) -> Self {
        Self {
            model,
            components,
            autonomy_profile: AutonomyProfileId::UnseenGroundNavigation,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedFacts {
    pub autonomy_profile: AutonomyProfileId,
    pub autonomy_profile_version: u32,
    pub localize_backend: LocalizeBackendKind,
    pub roles: Vec<ResolvedCapabilityRole>,
    pub conformance_report: ConformanceReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedCapabilityRole {
    pub capability: CapabilityRef,
    pub roles: Vec<Role>,
}

pub fn resolve_source_bundle(bundle: SourceBundle) -> Result<ResolvedFacts> {
    bundle.model.validate().map_err(|errors| {
        anyhow::anyhow!(
            "Robot errors:\n{}",
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        )
    })?;
    let profile = autonomy_profile(bundle.autonomy_profile);
    let role_resolution = resolve_roles(&bundle.model, &bundle.components)?;
    let localize_backend = resolve_localize_backend(&bundle.model, &bundle.components).kind();
    let conformance_report =
        validate_profile(&bundle.model, &role_resolution, bundle.autonomy_profile);
    if !conformance_report.is_pass() {
        bail!(
            "Autonomy profile conformance failed:\n{}",
            conformance_report
                .failures
                .iter()
                .map(|failure| format!("{}: {}", failure.check, failure.reason))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    Ok(ResolvedFacts {
        autonomy_profile: profile.id,
        autonomy_profile_version: profile.version,
        localize_backend,
        roles: flatten_roles(role_resolution),
        conformance_report,
    })
}

fn validate_profile(
    model: &Robot,
    roles: &RoleResolution,
    profile_id: AutonomyProfileId,
) -> ConformanceReport {
    let profile = autonomy_profile(profile_id);
    let mut evidence = Vec::new();
    let mut failures = Vec::new();

    let kinematic = model.motion.kinematic.kind();
    if profile.supported_kinematics.contains(&kinematic) {
        evidence.push(ConformanceEvidence::new(
            "kinematics",
            format!("{kinematic} is supported by {profile_id}"),
        ));
    } else {
        failures.push(ConformanceFailure::new(
            "kinematics",
            format!("{kinematic} is not supported by {profile_id}"),
        ));
    }

    evidence.push(ConformanceEvidence::new(
        "goal_pose_support",
        "planar profile accepts Pose2 only and rejects Pose3",
    ));

    for role in &profile.required_roles {
        let capabilities = roles.capabilities_for(*role);
        if capabilities.is_empty() {
            failures.push(ConformanceFailure::new(
                format!("role.{role}"),
                "required role has no resolved capability",
            ));
        } else {
            evidence.push(ConformanceEvidence::new(
                format!("role.{role}"),
                capabilities
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
    }

    if failures.is_empty() {
        ConformanceReport::pass(evidence)
    } else {
        ConformanceReport::fail(evidence, failures)
    }
}

fn flatten_roles(role_resolution: RoleResolution) -> Vec<ResolvedCapabilityRole> {
    role_resolution
        .assignments
        .into_iter()
        .map(|assignment| ResolvedCapabilityRole {
            capability: assignment.capability,
            roles: assignment.roles.into_iter().collect(),
        })
        .collect()
}
