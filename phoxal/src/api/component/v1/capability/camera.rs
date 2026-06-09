use crate::bus::pubsub::Stamped;
use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    Jpeg,
    Png,
    L8,
    Rgb8,
    Rgba8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    width: u32,
    height: u32,
    encoding: Encoding,
    intrinsics: Option<Intrinsics>,
    distortion: Option<Distortion>,
    exposure: Option<ExposureTiming>,
    measured_at_ns: Option<u64>,
    calibration: Option<CalibrationIdentity>,
    #[serde(with = "serde_bytes")]
    data: Vec<u8>,
}

impl Frame {
    pub fn new(width: u32, height: u32, encoding: Encoding, data: impl Into<Vec<u8>>) -> Self {
        Self {
            width,
            height,
            encoding,
            intrinsics: None,
            distortion: None,
            exposure: None,
            measured_at_ns: None,
            calibration: None,
            data: data.into(),
        }
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub const fn encoding(&self) -> Encoding {
        self.encoding
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Intrinsics {
    pub fx: f32,
    pub fy: f32,
    pub cx: f32,
    pub cy: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Distortion {
    pub model: String,
    pub coefficients: Vec<f32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ExposureTiming {
    pub exposure_start_ns: Option<u64>,
    pub exposure_duration_ns: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationIdentity {
    pub id: String,
    pub version: String,
}

impl TypedSchema for Frame {
    const SCHEMA_NAME: &'static str = "component/capability/camera";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "camera";

pub fn topic(
    bus: &crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> String {
    super::default_profile_topic(bus, component_id, capability_id)
}

pub fn subscriber_builder(
    bus: &crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> crate::bus::zenoh::TypedSubscriberBuilder<'_, '_, Stamped<Frame>> {
    crate::bus::pubsub::subscriber_builder(
        bus,
        &super::default_profile_path(component_id, capability_id),
    )
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::Frame;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Frame::SCHEMA_NAME, "component/capability/camera");
        assert_eq!(Frame::SCHEMA_VERSION, 1);
    }
}
