use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Localization,
    Mapping,
    Traversability,
    Safety,
    Odometry,
    Perception,
}

impl Role {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Localization => "localization",
            Self::Mapping => "mapping",
            Self::Traversability => "traversability",
            Self::Safety => "safety",
            Self::Odometry => "odometry",
            Self::Perception => "perception",
        }
    }

    #[must_use]
    pub const fn allows_multiple_capabilities(self) -> bool {
        matches!(
            self,
            Self::Localization
                | Self::Mapping
                | Self::Traversability
                | Self::Safety
                | Self::Perception
        )
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::Role;

    #[test]
    fn allows_multiple_capabilities_matches_blueprint_intent() {
        // Multi-input roles: aggregating contracts that fuse evidence.
        assert!(Role::Localization.allows_multiple_capabilities());
        assert!(Role::Mapping.allows_multiple_capabilities());
        assert!(Role::Traversability.allows_multiple_capabilities());
        assert!(Role::Safety.allows_multiple_capabilities());
        assert!(Role::Perception.allows_multiple_capabilities());

        // Single-input roles: one canonical capability per robot.
        assert!(!Role::Odometry.allows_multiple_capabilities());
    }
}
