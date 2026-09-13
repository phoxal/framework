const DEFAULT_MAX_AGE_MS: u64 = 100;

const DEFAULT_HISTORY_CAPACITY: u32 = 64;

const DEFAULT_FRAME_ID: &str = "odom";

const DEFAULT_WIDTH: u32 = 64;

const DEFAULT_HEIGHT: u32 = 64;

const DEFAULT_RESOLUTION_M: f64 = 0.1;

/// Typed, validated configuration for one bounded world instance.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct WorldConfig {
    /// Frame in which the admitted pose and generated window are expressed.
    #[serde(default = "default_frame_id")]
    pub frame_id: String,
    /// Lower-left x coordinate of the controlled map fixture.
    #[serde(default)]
    pub origin_x_m: f64,
    /// Lower-left y coordinate of the controlled map fixture.
    #[serde(default)]
    pub origin_y_m: f64,
    /// Number of cells along x.
    #[serde(default = "default_width")]
    pub width: u32,
    /// Number of cells along y.
    #[serde(default = "default_height")]
    pub height: u32,
    /// Cell resolution in metres.
    #[serde(default = "default_resolution_m")]
    pub resolution_m: f64,
    /// Maximum admitted pose age in logical milliseconds.
    #[serde(default = "default_max_age_ms")]
    pub max_age_ms: u64,
    /// Number of world snapshots retained for immutable reads.
    #[serde(default = "default_history_capacity")]
    pub history_capacity: u32,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            frame_id: default_frame_id(),
            origin_x_m: 0.0,
            origin_y_m: 0.0,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            resolution_m: DEFAULT_RESOLUTION_M,
            max_age_ms: DEFAULT_MAX_AGE_MS,
            history_capacity: DEFAULT_HISTORY_CAPACITY,
        }
    }
}

fn default_frame_id() -> String {
    DEFAULT_FRAME_ID.to_owned()
}

const fn default_width() -> u32 {
    DEFAULT_WIDTH
}

const fn default_height() -> u32 {
    DEFAULT_HEIGHT
}

const fn default_resolution_m() -> f64 {
    DEFAULT_RESOLUTION_M
}

const fn default_max_age_ms() -> u64 {
    DEFAULT_MAX_AGE_MS
}

const fn default_history_capacity() -> u32 {
    DEFAULT_HISTORY_CAPACITY
}

pub(super) fn validate_config(config: &WorldConfig) -> phoxal::Result<()> {
    if config.frame_id.is_empty() || config.frame_id.len() > phoxal_world::MAX_ID_BYTES {
        return Err(anyhow::anyhow!(
            "frame_id must contain 1 to {} UTF-8 bytes",
            phoxal_world::MAX_ID_BYTES
        ));
    }
    if !config.origin_x_m.is_finite() || !config.origin_y_m.is_finite() {
        return Err(anyhow::anyhow!("world origin must be finite"));
    }
    if config.width == 0 || config.height == 0 {
        return Err(anyhow::anyhow!("world width and height must be positive"));
    }
    if !config.resolution_m.is_finite() || config.resolution_m <= 0.0 {
        return Err(anyhow::anyhow!("resolution_m must be finite and positive"));
    }
    if config.max_age_ms == 0 || config.history_capacity == 0 {
        return Err(anyhow::anyhow!(
            "max_age_ms and history_capacity must be positive"
        ));
    }
    let cells = usize::try_from(config.width)
        .ok()
        .and_then(|width| {
            usize::try_from(config.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or_else(|| anyhow::anyhow!("world dimensions overflow"))?;
    if cells > 1_048_576 {
        return Err(anyhow::anyhow!(
            "world fixture is limited to 1,048,576 cells"
        ));
    }
    let max_x_m = config.origin_x_m + f64::from(config.width) * config.resolution_m;
    let max_y_m = config.origin_y_m + f64::from(config.height) * config.resolution_m;
    if !max_x_m.is_finite() || !max_y_m.is_finite() {
        return Err(anyhow::anyhow!("world bounds must be finite"));
    }
    Ok(())
}
