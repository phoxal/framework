use serde::{Deserialize, Serialize};

use crate::scenario::definition::ScenarioSpec;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub spec: ScenarioSpec,
    pub outcome: ScenarioOutcome,
    pub elapsed_logical_ns: Option<u64>,
    pub failure_diagnostics: Vec<String>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScenarioOutcome {
    Passed,
    Failed,
    MissingImplementation,
    Timeout,
    TeardownError,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScenarioReport {
    pub selector: String,
    pub results: Vec<ScenarioResult>,
}

impl ScenarioReport {
    pub fn new(selector: impl Into<String>, results: Vec<ScenarioResult>) -> Self {
        Self {
            selector: selector.into(),
            results,
        }
    }

    pub fn has_failures(&self) -> bool {
        self.results
            .iter()
            .any(|result| result.outcome != ScenarioOutcome::Passed)
    }

    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}
