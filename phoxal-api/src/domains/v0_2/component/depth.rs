//! Checked v0.2 depth frames.

#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    U16Millimeters,
}
#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvalidSamplePolicy {
    ZeroIsInvalid,
    NonFiniteIsInvalid,
}
#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Intrinsics {
    pub fx: f32,
    pub fy: f32,
    pub cx: f32,
    pub cy: f32,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Distortion {
    pub model: String,
    pub coefficients: Vec<f32>,
}
#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExposureTiming {
    pub exposure_start_ns: Option<u64>,
    pub exposure_duration_ns: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CalibrationIdentity {
    pub id: String,
    pub version: String,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "FrameWire")]
pub struct Frame {
    pub samples_mm: Vec<u16>,
    pub encoding: Encoding,
    pub invalid_sample_policy: InvalidSamplePolicy,
    pub width: u32,
    pub height: u32,
    pub intrinsics: Option<Intrinsics>,
    pub distortion: Option<Distortion>,
    pub exposure: Option<ExposureTiming>,
    pub calibration: Option<CalibrationIdentity>,
}
#[derive(serde::Deserialize)]
struct FrameWire {
    samples_mm: Vec<u16>,
    encoding: Encoding,
    invalid_sample_policy: InvalidSamplePolicy,
    width: u32,
    height: u32,
    intrinsics: Option<Intrinsics>,
    distortion: Option<Distortion>,
    exposure: Option<ExposureTiming>,
    calibration: Option<CalibrationIdentity>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidFrame(&'static str);
impl std::fmt::Display for InvalidFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for InvalidFrame {}
impl Frame {
    pub fn try_new(
        samples_mm: Vec<u16>,
        encoding: Encoding,
        invalid_sample_policy: InvalidSamplePolicy,
        width: u32,
        height: u32,
        intrinsics: Option<Intrinsics>,
        distortion: Option<Distortion>,
        exposure: Option<ExposureTiming>,
        calibration: Option<CalibrationIdentity>,
    ) -> Result<Self, InvalidFrame> {
        let expected = usize::try_from(width)
            .ok()
            .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
            .ok_or(InvalidFrame("depth dimensions overflow"))?;
        if width == 0 || height == 0 || samples_mm.len() != expected {
            return Err(InvalidFrame(
                "depth dimensions must be nonzero and match sample count",
            ));
        }
        if let Some(i) = intrinsics {
            if !(i.fx.is_finite()
                && i.fy.is_finite()
                && i.fx > 0.0
                && i.fy > 0.0
                && i.cx.is_finite()
                && i.cy.is_finite())
            {
                return Err(InvalidFrame(
                    "depth intrinsics must be finite with positive focal lengths",
                ));
            }
        }
        if distortion.as_ref().is_some_and(|d| {
            d.model.trim().is_empty() || !d.coefficients.iter().all(|v| v.is_finite())
        }) {
            return Err(InvalidFrame("depth distortion must be named and finite"));
        }
        if calibration
            .as_ref()
            .is_some_and(|c| c.id.trim().is_empty() || c.version.trim().is_empty())
        {
            return Err(InvalidFrame("depth calibration identity must be nonempty"));
        }
        Ok(Self {
            samples_mm,
            encoding,
            invalid_sample_policy,
            width,
            height,
            intrinsics,
            distortion,
            exposure,
            calibration,
        })
    }
}
impl TryFrom<FrameWire> for Frame {
    type Error = InvalidFrame;
    fn try_from(v: FrameWire) -> Result<Self, Self::Error> {
        Self::try_new(
            v.samples_mm,
            v.encoding,
            v.invalid_sample_policy,
            v.width,
            v.height,
            v.intrinsics,
            v.distortion,
            v.exposure,
            v.calibration,
        )
    }
}
