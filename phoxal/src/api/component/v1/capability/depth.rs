use crate::bus::pubsub::Stamped;
use crate::bus::zenoh_typed::TypedSchema;
use serde::{Deserialize, Serialize};

pub const MILLIMETERS_PER_METER: f32 = 1000.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Depth {
    /// Dense depth samples encoded as unsigned 16-bit millimeters.
    ///
    /// Contract:
    /// - resolution is static component metadata, not packet data
    /// - each sample stores perspective depth along the sensor forward axis,
    ///   not radial ray length
    /// - samples are row-major over the static depth grid
    /// - columns increase to the sensor's right and rows increase downward
    /// - producers publish only complete grids with valid non-zero samples
    /// - the camera frame uses +X forward, +Y left, +Z up after the producer's
    ///   configured sensor transform is applied into the robot frame
    ///
    /// Producers and consumers must treat this as one shared geometry rule so
    /// Webots and real hardware publish compatible depth data.
    samples_mm: Vec<u16>,
    encoding: Encoding,
    invalid_sample_policy: InvalidSamplePolicy,
    width: Option<u32>,
    height: Option<u32>,
    intrinsics: Option<super::camera::Intrinsics>,
    distortion: Option<super::camera::Distortion>,
    exposure: Option<super::camera::ExposureTiming>,
    measured_at_ns: Option<u64>,
    calibration: Option<super::camera::CalibrationIdentity>,
}

impl Depth {
    pub fn new(samples_mm: Vec<u16>) -> Self {
        Self {
            samples_mm,
            encoding: Encoding::U16Millimeters,
            invalid_sample_policy: InvalidSamplePolicy::ZeroIsInvalid,
            width: None,
            height: None,
            intrinsics: None,
            distortion: None,
            exposure: None,
            measured_at_ns: None,
            calibration: None,
        }
    }

    pub fn with_resolution(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    pub fn from_meters(data_m: impl IntoIterator<Item = f32>) -> Option<Self> {
        let samples_mm = data_m
            .into_iter()
            .map(meters_to_sample_mm)
            .collect::<Option<Vec<_>>>()?;
        (!samples_mm.is_empty()).then_some(Self::new(samples_mm))
    }

    pub fn samples_mm(&self) -> &[u16] {
        &self.samples_mm
    }

    pub const fn width(&self) -> Option<u32> {
        self.width
    }

    pub const fn height(&self) -> Option<u32> {
        self.height
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    U16Millimeters,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvalidSamplePolicy {
    ZeroIsInvalid,
    NonFiniteIsInvalid,
}

fn meters_to_sample_mm(value_m: f32) -> Option<u16> {
    if value_m.is_finite() && value_m > 0.0 {
        let sample_mm = (value_m * MILLIMETERS_PER_METER).round();
        if sample_mm <= f32::from(u16::MAX) {
            Some(sample_mm.max(1.0) as u16)
        } else {
            None
        }
    } else {
        None
    }
}

impl TypedSchema for Depth {
    const SCHEMA_NAME: &'static str = "component/capability/depth";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "depth";

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
) -> crate::bus::zenoh_typed::TypedSubscriberBuilder<'_, '_, Stamped<Depth>> {
    crate::bus::pubsub::subscriber_builder(
        bus,
        &super::default_profile_path(component_id, capability_id),
    )
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh_typed::TypedSchema;

    use super::{Depth, meters_to_sample_mm};

    #[test]
    fn converts_meters_to_millimeter_samples() {
        assert_eq!(meters_to_sample_mm(1.234), Some(1234));
        assert_eq!(meters_to_sample_mm(0.0004), Some(1));
        assert_eq!(meters_to_sample_mm(f32::NAN), None);
        assert_eq!(meters_to_sample_mm(-1.0), None);
        assert_eq!(meters_to_sample_mm(100.0), None);
    }

    #[test]
    fn builds_depth_from_meters() {
        let depth = Depth::from_meters([0.5, 2.0]).expect("valid depth");

        assert_eq!(depth.samples_mm(), &[500, 2000]);
    }

    #[test]
    fn resolution_is_carried_when_producer_sets_it() {
        let depth = Depth::new(vec![1_000; 320 * 240]).with_resolution(320, 240);

        assert_eq!(depth.width(), Some(320));
        assert_eq!(depth.height(), Some(240));
    }

    #[test]
    fn rejects_invalid_meter_samples() {
        assert!(Depth::from_meters([0.5, f32::NAN]).is_none());
        assert!(Depth::from_meters([]).is_none());
    }

    #[test]
    fn depth_schema_rewrites_v1_contract() {
        assert_eq!(Depth::SCHEMA_NAME, "component/capability/depth");
        assert_eq!(Depth::SCHEMA_VERSION, 1);
    }
}
