/// Admitted navigation work and arrival policy for one robot instance.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct NavigationConfig {
    /// Maximum bounded search expansions performed by each invocation.
    pub max_expansions_per_step: u32,
    /// Maximum measured planar distance considered reached, in metres.
    pub goal_tolerance_m: f64,
}

pub(super) fn validate_navigation_config(config: &NavigationConfig) -> phoxal::Result<()> {
    if config.max_expansions_per_step == 0 {
        return Err(anyhow::anyhow!("max_expansions_per_step must be positive"));
    }
    if !config.goal_tolerance_m.is_finite() || config.goal_tolerance_m <= 0.0 {
        return Err(anyhow::anyhow!(
            "goal_tolerance_m must be finite and positive"
        ));
    }
    Ok(())
}
