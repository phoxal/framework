use crate::capabilities::publish_every_steps;
use anyhow::{Result, anyhow};
use phoxal::api::component::capability::lidar::v1::{
    Points as LidarPointsData, Ranges as LidarRangesData, Scan as LidarData,
};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanMode {
    Ranges,
    Points,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    pub publish_rate_hz: f64,
    pub sampling_period_hz: f64,
    pub output: ScanMode,
}

pub struct Lidar {
    lidar: webots_rs::device::lidar::Lidar,
    output: ScanMode,
    publish_every_steps: u64,
}

impl Lidar {
    pub fn new(
        lidar: webots_rs::device::lidar::Lidar,
        basic_time_step_ms: i32,
        sample_period_ms: i32,
        config: &Config,
    ) -> Result<Self> {
        lidar
            .enable(sample_period_ms)
            .map_err(|error| anyhow!(error))?;

        Ok(Self {
            lidar,
            output: config.output,
            publish_every_steps: publish_every_steps(basic_time_step_ms, config.publish_rate_hz)?,
        })
    }

    pub fn read_if_due(&self, step_count: u64, _time_ns: u64) -> Result<Option<LidarData>> {
        if !step_count.is_multiple_of(self.publish_every_steps) {
            return Ok(None);
        }

        let data = match self.output {
            ScanMode::Ranges => {
                let ranges = self
                    .lidar
                    .get_layer_range_image(0)
                    .map_err(|error| anyhow!(error))?;
                LidarData::Ranges(LidarRangesData::new(ranges))
            }
            ScanMode::Points => {
                let reading = self.lidar.reading().map_err(|error| anyhow!(error))?;
                let webots_rs::device::lidar::LidarReading::PointCloud(points) = reading else {
                    anyhow::bail!("expected point cloud reading from lidar");
                };
                LidarData::Points(LidarPointsData::new(
                    points
                        .into_iter()
                        .map(|point| [point.x, point.y, point.z])
                        .collect::<Vec<_>>(),
                ))
            }
        };

        Ok(Some(data))
    }
}
