//! Camera capability: publishes `component::camera::Frame` from the Webots
//! `Camera` device. Moved from the monolith's `CameraSpec` (main.rs:585-591),
//! `NativeCamera` (main.rs:1370-1414), and the BGRA conversion helpers
//! (main.rs:1536-1551).

use anyhow::{Result, anyhow};
use phoxal::model::component::v0::capability::CameraMode;
use phoxal_api::v1 as api;

use super::{SampledSpec, is_due};

#[derive(Clone, Debug)]
pub(crate) struct CameraSpec {
    pub(crate) sampled: SampledSpec,
    pub(crate) mode: CameraMode,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

pub(crate) struct NativeCamera {
    camera: webots_rs::device::camera::Camera,
    spec: CameraSpec,
}

impl NativeCamera {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &CameraSpec) -> Result<Self> {
        let camera = webots
            .camera(spec.sampled.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        camera
            .enable(spec.sampled.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            camera,
            spec: spec.clone(),
        })
    }

    pub(crate) fn read_if_due(
        &self,
        step_index: u64,
        time_ns: u64,
    ) -> Result<Option<api::component::camera::Frame>> {
        if !is_due(step_index, self.spec.sampled.publish_every_steps) {
            return Ok(None);
        }
        let bgra = self.camera.get_image().map_err(|error| anyhow!(error))?;
        let (encoding, data) = match self.spec.mode {
            CameraMode::Mono => (api::component::camera::Encoding::L8, bgra_to_luma(&bgra)),
            CameraMode::Rgb => (api::component::camera::Encoding::Rgb8, bgra_to_rgb(&bgra)),
        };
        Ok(Some(api::component::camera::Frame {
            width: self.spec.width,
            height: self.spec.height,
            encoding,
            intrinsics: None,
            distortion: None,
            exposure: None,
            measured_at_ns: Some(time_ns),
            calibration: None,
            data,
        }))
    }
}

fn bgra_to_rgb(bgra: &[u8]) -> Vec<u8> {
    bgra.chunks_exact(4)
        .flat_map(|pixel| [pixel[2], pixel[1], pixel[0]])
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
    use super::*;

    #[test]
    fn bgra_to_rgb_swaps_channel_order() {
        assert_eq!(bgra_to_rgb(&[10, 20, 30, 255]), vec![30, 20, 10]);
    }

    #[test]
    fn bgra_to_luma_applies_bt601_weights() {
        assert_eq!(bgra_to_luma(&[10, 20, 30, 255]), vec![21]);
    }
}
