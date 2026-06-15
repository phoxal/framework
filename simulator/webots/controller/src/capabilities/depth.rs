use crate::capabilities::publish_every_steps;
use anyhow::{Result, anyhow};
use phoxal::api::component::capability::depth::v1::Depth as DepthPayload;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    pub publish_rate_hz: f64,
    pub sampling_period_hz: f64,
}

pub struct Depth {
    sensor: webots_rs::device::range_finder::RangeFinder,
    width: u32,
    height: u32,
    publish_every_steps: u64,
}

impl Depth {
    pub fn new(
        sensor: webots_rs::device::range_finder::RangeFinder,
        basic_time_step_ms: i32,
        sample_period_ms: i32,
        config: &Config,
    ) -> Result<Self> {
        sensor
            .enable(sample_period_ms)
            .map_err(|error| anyhow!(error))?;
        let width = sensor.get_width().map_err(|error| anyhow!(error))? as u32;
        let height = sensor.get_height().map_err(|error| anyhow!(error))? as u32;

        Ok(Self {
            sensor,
            width,
            height,
            publish_every_steps: publish_every_steps(basic_time_step_ms, config.publish_rate_hz)?,
        })
    }

    pub fn read_if_due(&self, step_count: u64, _time_ns: u64) -> Result<Option<DepthPayload>> {
        if !step_count.is_multiple_of(self.publish_every_steps) {
            return Ok(None);
        }

        // This is intentionally a simplified Webots-native depth approximation
        // for OAK-D Lite v1, not a controller-side stereo disparity pipeline.
        let image_m = self
            .sensor
            .get_range_image()
            .map_err(|error| anyhow!(error))?;
        Ok(DepthPayload::from_meters(image_m)
            .map(|depth| depth.with_resolution(self.width, self.height)))
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub const fn publish_every_steps(&self) -> u64 {
        self.publish_every_steps
    }
}
