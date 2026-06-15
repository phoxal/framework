use crate::capabilities::publish_every_steps;
use anyhow::{Result, anyhow};
use phoxal::api::component::capability::camera::v1::Encoding as ImageEncoding;
use phoxal::api::component::capability::camera::v1::Frame;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraMode {
    Mono,
    Rgb,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    pub mode: CameraMode,
    pub publish_rate_hz: f64,
    pub sampling_period_hz: f64,
}

pub struct Camera {
    camera: webots_rs::device::camera::Camera,
    mode: CameraMode,
    width: u32,
    height: u32,
    publish_every_steps: u64,
}

impl Camera {
    pub fn new(
        camera: webots_rs::device::camera::Camera,
        basic_time_step_ms: i32,
        sample_period_ms: i32,
        config: &Config,
    ) -> Result<Self> {
        let width = camera.get_width().map_err(|error| anyhow!(error))? as u32;
        let height = camera.get_height().map_err(|error| anyhow!(error))? as u32;

        camera
            .enable(sample_period_ms)
            .map_err(|error| anyhow!(error))?;

        Ok(Self {
            camera,
            mode: config.mode,
            width,
            height,
            publish_every_steps: publish_every_steps(basic_time_step_ms, config.publish_rate_hz)?,
        })
    }

    pub fn read_if_due(&mut self, step_count: u64, _time_ns: u64) -> Result<Option<Frame>> {
        if !step_count.is_multiple_of(self.publish_every_steps) {
            return Ok(None);
        }

        let bgra = self.camera.get_image().map_err(|error| anyhow!(error))?;
        let (encoding, data) = match self.mode {
            CameraMode::Mono => (ImageEncoding::L8, bgra_to_luma(&bgra)),
            CameraMode::Rgb => (ImageEncoding::Rgba8, bgra_to_rgba(&bgra)),
        };
        Ok(Some(Frame::new(self.width, self.height, encoding, data)))
    }

    pub const fn publish_every_steps(&self) -> u64 {
        self.publish_every_steps
    }
}

fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    bgra.chunks_exact(4)
        .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], pixel[3]])
        .collect()
}

fn bgra_to_luma(bgra: &[u8]) -> Vec<u8> {
    bgra.chunks_exact(4)
        .map(|pixel| {
            let red = u32::from(pixel[2]);
            let green = u32::from(pixel[1]);
            let blue = u32::from(pixel[0]);
            ((299 * red + 587 * green + 114 * blue) / 1000) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{bgra_to_luma, bgra_to_rgba};

    #[test]
    fn converts_webots_bgra_to_contract_rgba() {
        let rgba = bgra_to_rgba(&[10, 20, 30, 255, 40, 50, 60, 128]);

        assert_eq!(rgba, vec![30, 20, 10, 255, 60, 50, 40, 128]);
    }

    #[test]
    fn converts_webots_bgra_to_luma_using_rgb_weights() {
        let luma = bgra_to_luma(&[10, 20, 30, 255]);

        assert_eq!(luma, vec![21]);
    }
}
