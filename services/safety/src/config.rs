use std::collections::BTreeSet;

/// Protective interpretation of one required range source.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct RangeRequirement {
    /// Source identity carried by the connected range sample.
    pub sensor_id: String,
    /// A return at or below this distance requests a stop.
    #[serde(default = "default_stop_distance")]
    pub protective_stop_distance_m: f64,
    /// Optional upper bound for required ground or other supporting surfaces.
    /// A farther return does not establish that the required surface exists.
    #[serde(default)]
    pub maximum_clear_distance_m: Option<f64>,
    /// Optional proximity band above the stop distance that limits speed.
    #[serde(default = "default_proximity_distance")]
    pub proximity_limit_distance_m: Option<f64>,
}

/// Typed, validated policy for one Safety instance.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct SafetyConfig {
    /// Maximum age admitted for world, motion, and range evidence.
    #[serde(default = "default_input_max_age_ms")]
    pub input_max_age_ms: u64,
    /// Lifetime of each emitted protective constraints product.
    #[serde(default = "default_constraint_ttl_ms")]
    pub constraint_ttl_ms: u64,
    /// Forward speed limit emitted for a proximity constraint.
    #[serde(default = "default_proximity_linear_limit_mps")]
    pub proximity_linear_limit_mps: f64,
    /// At most 64 distinct required sources and their protective distance bounds.
    #[serde(default)]
    pub ranges: Vec<RangeRequirement>,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            input_max_age_ms: default_input_max_age_ms(),
            constraint_ttl_ms: default_constraint_ttl_ms(),
            proximity_linear_limit_mps: default_proximity_linear_limit_mps(),
            ranges: Vec::new(),
        }
    }
}

const fn default_input_max_age_ms() -> u64 {
    100
}
const fn default_constraint_ttl_ms() -> u64 {
    300
}
const fn default_stop_distance() -> f64 {
    0.25
}
const fn default_proximity_distance() -> Option<f64> {
    Some(0.60)
}
const fn default_proximity_linear_limit_mps() -> f64 {
    0.15
}

pub(super) fn validate_config(config: &SafetyConfig) -> phoxal::Result<()> {
    if config.ranges.len() > 64 {
        return Err(anyhow::anyhow!(
            "at most 64 required range sources are supported"
        ));
    }
    let mut ids = BTreeSet::new();
    for range in &config.ranges {
        if range.sensor_id.is_empty()
            || range.sensor_id.len() > 64
            || !range
                .sensor_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            || !ids.insert(&range.sensor_id)
        {
            return Err(anyhow::anyhow!(
                "range source IDs must be distinct and contain 1 to 64 identifier bytes"
            ));
        }
        if !range.protective_stop_distance_m.is_finite() || range.protective_stop_distance_m < 0.0 {
            return Err(anyhow::anyhow!(
                "protective stop distance must be finite and nonnegative"
            ));
        }
        for distance in [
            range.maximum_clear_distance_m,
            range.proximity_limit_distance_m,
        ]
        .into_iter()
        .flatten()
        {
            if !distance.is_finite() || distance <= range.protective_stop_distance_m {
                return Err(anyhow::anyhow!(
                    "clear and proximity bounds must be finite and above the stop distance"
                ));
            }
        }
        if let (Some(proximity), Some(maximum)) = (
            range.proximity_limit_distance_m,
            range.maximum_clear_distance_m,
        ) && proximity > maximum
        {
            return Err(anyhow::anyhow!(
                "proximity limit must not exceed the maximum clear distance"
            ));
        }
    }
    if config.input_max_age_ms == 0 || config.constraint_ttl_ms == 0 {
        return Err(anyhow::anyhow!(
            "input age and constraint lifetime must be positive"
        ));
    }
    if !config.proximity_linear_limit_mps.is_finite() || config.proximity_linear_limit_mps <= 0.0 {
        return Err(anyhow::anyhow!(
            "proximity speed must be finite and positive"
        ));
    }
    Ok(())
}
