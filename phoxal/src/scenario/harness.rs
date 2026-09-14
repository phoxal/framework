//! Case-host protocol skeleton (P1).
//!
//! P1 ships a stub `run_harness` that drives one scenario through the
//! registry and returns a [`HarnessRun`]. P2-P5 will replace the stub body
//! with the real supervisor + simulator orchestration described in the
//! plan.

use thiserror::Error;

use crate::scenario::registry::ScenarioOutcome;

/// Failure modes the case host can return.
#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("scenario `{0}` is not registered")]
    UnknownScenario(String),
    #[error("{0}")]
    DuplicateIdentity(crate::scenario::registry::DuplicateScenarioError),
    #[error("scenario `{0}` failed: {1}")]
    ScenarioFailed(String, String),
    #[error("internal harness error: {0}")]
    Internal(String),
}

/// Aggregated result of one harness invocation. P3/P5 will populate
/// `report_artifact_path` with the sealed case report; P1 leaves it
/// `None`.
#[derive(Debug, Clone)]
pub struct HarnessRun {
    pub name: String,
    pub passed: bool,
    pub detail: Option<String>,
    pub report_artifact_path: Option<String>,
}

/// P1 stub harness entry point. The case-host protocol, supervisor
/// lifecycle, simulator provisioning, and typed evidence sealing all
/// land in later phases; this is just enough to prove the registry ->
/// dispatch path compiles and behaves deterministically.
///
/// Returns `crate::Result` (anyhow) so the generated `phoxal-scenarios`
/// test target can `?`-propagate directly without an `Into` impl.
pub fn run_harness(short_name: &str) -> crate::Result<HarnessRun> {
    let entries = crate::scenario::registry::list_scenarios().map_err(|e| crate::anyhow!("{e}"))?;

    let entry = entries
        .iter()
        .find(|e| e.short_name == short_name)
        .ok_or_else(|| {
            crate::anyhow!("{}", HarnessError::UnknownScenario(short_name.to_owned()))
        })?;

    let outcome: ScenarioOutcome = (entry.entry)()
        .map_err(|e| crate::anyhow!("{}", HarnessError::Internal(format!("{e:#}"))))?;
    let run = HarnessRun {
        name: outcome.name.clone(),
        passed: outcome.passed,
        detail: outcome.detail.clone(),
        report_artifact_path: None,
    };
    if !run.passed {
        return Err(crate::anyhow!(
            "{}",
            HarnessError::ScenarioFailed(run.name.clone(), run.detail.clone().unwrap_or_default(),)
        ));
    }
    Ok(run)
}
