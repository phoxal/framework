use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{KinematicKind, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyProfileId {
    UnseenGroundNavigation,
    /// Outdoor night variant that inherits `UnseenGroundNavigation` and tightens it
    /// (near-field proximity alongside RGB-D, controllable illumination, a GNSS
    /// global-frame anchor or recorded waiver, wider dead-reckoning budgets, a lower
    /// Tracking speed cap, and night scenario coverage). Selecting it implicitly
    /// selects its parent; the conformance report covers the full chain.
    UnseenGroundNavigationNight,
}

impl AutonomyProfileId {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnseenGroundNavigation => "unseen_ground_navigation",
            Self::UnseenGroundNavigationNight => "unseen_ground_navigation_night",
        }
    }

    /// The parent profile this one extends, if any. A child adds requirements,
    /// tightens thresholds, or supplies policy values; it never removes a parent
    /// requirement.
    #[must_use]
    pub const fn parent(self) -> Option<Self> {
        match self {
            Self::UnseenGroundNavigation => None,
            Self::UnseenGroundNavigationNight => Some(Self::UnseenGroundNavigation),
        }
    }
}

impl std::fmt::Display for AutonomyProfileId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalPoseSupport {
    Pose2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutonomyProfileSpec {
    pub id: AutonomyProfileId,
    pub version: u32,
    pub required_roles: BTreeSet<Role>,
    pub supported_kinematics: BTreeSet<KinematicKind>,
    pub goal_pose_support: GoalPoseSupport,
    pub required_sensing: Vec<String>,
    pub safety_envelope: String,
    pub localization_capability: String,
    pub world_model_capability: String,
    pub scenario_coverage: Vec<String>,
    pub policy_defaults: Vec<String>,
}

#[must_use]
pub fn autonomy_profile(id: AutonomyProfileId) -> AutonomyProfileSpec {
    match id {
        AutonomyProfileId::UnseenGroundNavigation => AutonomyProfileSpec {
            id,
            version: 1,
            required_roles: [
                Role::Localization,
                Role::Mapping,
                Role::Traversability,
                Role::Safety,
                Role::Odometry,
            ]
            .into_iter()
            .collect(),
            supported_kinematics: [KinematicKind::Differential].into_iter().collect(),
            goal_pose_support: GoalPoseSupport::Pose2,
            required_sensing: vec![
                "imu".to_string(),
                "depth_or_lidar".to_string(),
                "near_field_range_depth_or_lidar".to_string(),
                "wheel_or_drive_feedback".to_string(),
            ],
            safety_envelope: "profile_policy_defaults".to_string(),
            localization_capability: "localization".to_string(),
            world_model_capability: "map_owned_traversability".to_string(),
            scenario_coverage: vec![
                "boot-contract".to_string(),
                "frame-calibration".to_string(),
                "odometry".to_string(),
                "localization".to_string(),
                "mapping".to_string(),
                "traversability".to_string(),
                "safety".to_string(),
            ],
            policy_defaults: vec![
                "localization_policy".to_string(),
                "map_revision_retention".to_string(),
                "safety_profile".to_string(),
                "runtime_timing".to_string(),
            ],
        },
        // Night inherits the base profile and tightens it (BLUEPRINT "autonomy profile"):
        // it adds requirements / policy values and never removes a parent requirement.
        AutonomyProfileId::UnseenGroundNavigationNight => {
            let mut spec = autonomy_profile(AutonomyProfileId::UnseenGroundNavigation);
            spec.id = id;
            // Near-field proximity alongside RGB-D, controllable illumination, and a GNSS
            // global-frame anchor (or a recorded waiver) are required at night.
            for sensing in ["controllable_illumination", "gnss_global_anchor_or_waiver"] {
                let sensing = sensing.to_string();
                if !spec.required_sensing.contains(&sensing) {
                    spec.required_sensing.push(sensing);
                }
            }
            spec.scenario_coverage.push("night-log-replay".to_string());
            // Wider dead-reckoning budgets, a lower Tracking speed cap, and an illumination
            // policy are supplied as night policy values.
            for policy in [
                "night_dead_reckoning_budget",
                "night_tracking_speed_cap",
                "night_illumination_policy",
            ] {
                spec.policy_defaults.push(policy.to_string());
            }
            spec
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AutonomyProfileId, autonomy_profile};

    #[test]
    fn night_profile_extends_base_without_removing_requirements() {
        assert_eq!(
            AutonomyProfileId::UnseenGroundNavigationNight.parent(),
            Some(AutonomyProfileId::UnseenGroundNavigation)
        );
        assert_eq!(
            AutonomyProfileId::UnseenGroundNavigationNight.as_str(),
            "unseen_ground_navigation_night"
        );

        let base = autonomy_profile(AutonomyProfileId::UnseenGroundNavigation);
        let night = autonomy_profile(AutonomyProfileId::UnseenGroundNavigationNight);

        // A child never removes a parent requirement.
        assert!(base.required_roles.is_subset(&night.required_roles));
        for sensing in &base.required_sensing {
            assert!(
                night.required_sensing.contains(sensing),
                "night profile dropped base sensing requirement {sensing}"
            );
        }
        // It tightens with night-specific requirements and policy values.
        for added in ["controllable_illumination", "gnss_global_anchor_or_waiver"] {
            assert!(night.required_sensing.iter().any(|s| s == added));
        }
        assert!(
            night
                .scenario_coverage
                .iter()
                .any(|s| s == "night-log-replay")
        );
        for policy in ["night_tracking_speed_cap", "night_dead_reckoning_budget"] {
            assert!(night.policy_defaults.iter().any(|s| s == policy));
        }
        assert_eq!(night.id, AutonomyProfileId::UnseenGroundNavigationNight);
    }
}
