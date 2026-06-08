use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScenarioSpec {
    pub name: String,
    pub world: WebotsWorld,
    pub test: String,
    pub timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixture_bundle: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WebotsWorld {
    name: String,
}

impl WebotsWorld {
    pub fn named(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub fn as_str(&self) -> &str {
        &self.name
    }
}

impl ScenarioSpec {
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            test: name.clone(),
            name,
            world: WebotsWorld::named("headless"),
            timeout_secs: 30,
            category: None,
            tier: None,
            fixture_bundle: None,
        }
    }

    pub fn world(mut self, world: WebotsWorld) -> Self {
        self.world = world;
        self
    }

    pub fn test(mut self, test: impl Into<String>) -> Self {
        self.test = test.into();
        self
    }

    pub fn timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    pub fn category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    pub fn tier(mut self, tier: u8) -> Self {
        self.tier = Some(tier);
        self
    }

    pub fn fixture_bundle(mut self, fixture_bundle: impl Into<String>) -> Self {
        self.fixture_bundle = Some(fixture_bundle.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{ScenarioSpec, WebotsWorld};

    #[test]
    fn scenario_spec_builds_robot_acceptance_shape() {
        let spec = ScenarioSpec::new("drive-forward")
            .world(WebotsWorld::named("ArenaWorld"))
            .test("drive_forward")
            .timeout_secs(30)
            .category("directed-drive")
            .tier(1);

        assert_eq!(spec.name, "drive-forward");
        assert_eq!(spec.world.as_str(), "ArenaWorld");
        assert_eq!(spec.test, "drive_forward");
        assert_eq!(spec.timeout_secs, 30);
        assert_eq!(spec.category.as_deref(), Some("directed-drive"));
        assert_eq!(spec.tier, Some(1));
        assert_eq!(spec.fixture_bundle, None);
    }

    #[test]
    fn webots_world_serializes_as_name() {
        let json = serde_json::to_string(&WebotsWorld::named("SimpleWorld")).unwrap();

        assert_eq!(json, "\"SimpleWorld\"");
    }
}
