use crate::capabilities::publish_every_steps;
use anyhow::{Result, anyhow};
use phoxal::api::v1::component::capability::mmwave::{
    Detection as MmwaveDetection, Scan as MmwaveData,
};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    pub publish_rate_hz: f64,
    pub sampling_period_hz: f64,
}

pub struct Mmwave {
    radar: webots_rs::device::radar::Radar,
    publish_every_steps: u64,
}

impl Mmwave {
    pub fn new(
        radar: webots_rs::device::radar::Radar,
        basic_time_step_ms: i32,
        sample_period_ms: i32,
        config: &Config,
    ) -> Result<Self> {
        radar
            .enable(sample_period_ms)
            .map_err(|error| anyhow!(error))?;

        Ok(Self {
            radar,
            publish_every_steps: publish_every_steps(basic_time_step_ms, config.publish_rate_hz)?,
        })
    }

    pub fn read_if_due(&self, step_count: u64, _time_ns: u64) -> Result<Option<MmwaveData>> {
        if !step_count.is_multiple_of(self.publish_every_steps) {
            return Ok(None);
        }

        let targets = self.radar.targets().map_err(|error| anyhow!(error))?;
        let detections: Vec<_> = targets
            .into_iter()
            .map(|target| {
                let x = target.distance * target.azimuth.cos();
                let y = target.distance * target.azimuth.sin();
                let vx = target.speed * target.azimuth.cos();
                let vy = target.speed * target.azimuth.sin();

                MmwaveDetection::new(
                    [x as f32, -(y as f32), 0.0],
                    [vx as f32, -(vy as f32), 0.0],
                    target.received_power as f32,
                )
            })
            .collect();

        Ok(Some(MmwaveData::new(detections)))
    }
}
