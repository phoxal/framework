#[derive(
    phoxal_macros::DescribeWire,
    Copy,
    Eq,
    Clone,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    Jpeg,
    Png,
    L8,
    Rgb8,
    Rgba8,
}

#[derive(
    phoxal_macros::DescribeWire, Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct Intrinsics {
    pub fx: f32,
    pub fy: f32,
    pub cx: f32,
    pub cy: f32,
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct Distortion {
    pub model: String,
    pub coefficients: Vec<f32>,
}

#[derive(
    phoxal_macros::DescribeWire, Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct ExposureTiming {
    pub exposure_start_ns: Option<u64>,
    pub exposure_duration_ns: Option<u64>,
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct CalibrationIdentity {
    pub id: String,
    pub version: String,
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "FrameWire")]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub encoding: Encoding,
    pub intrinsics: Option<Intrinsics>,
    pub distortion: Option<Distortion>,
    pub exposure: Option<ExposureTiming>,
    pub calibration: Option<CalibrationIdentity>,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

#[derive(serde::Deserialize)]
struct FrameWire {
    width: u32,
    height: u32,
    encoding: Encoding,
    intrinsics: Option<Intrinsics>,
    distortion: Option<Distortion>,
    exposure: Option<ExposureTiming>,
    calibration: Option<CalibrationIdentity>,
    #[serde(with = "serde_bytes")]
    data: Vec<u8>,
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
    #[expect(
        clippy::too_many_arguments,
        reason = "the validating constructor receives the complete wire frame atomically"
    )]
    pub fn try_new(
        width: u32,
        height: u32,
        encoding: Encoding,
        intrinsics: Option<Intrinsics>,
        distortion: Option<Distortion>,
        exposure: Option<ExposureTiming>,
        calibration: Option<CalibrationIdentity>,
        data: Vec<u8>,
    ) -> Result<Self, InvalidFrame> {
        if width == 0 || height == 0 {
            return Err(InvalidFrame("camera dimensions must be nonzero"));
        }
        let pixels = usize::try_from(width)
            .ok()
            .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
            .ok_or(InvalidFrame("camera dimensions overflow"))?;
        match encoding {
            Encoding::L8 | Encoding::Rgb8 | Encoding::Rgba8 => {
                let channels = match encoding {
                    Encoding::L8 => 1,
                    Encoding::Rgb8 => 3,
                    Encoding::Rgba8 => 4,
                    _ => unreachable!(),
                };
                if data.len()
                    != pixels
                        .checked_mul(channels)
                        .ok_or(InvalidFrame("camera image shape overflows"))?
                {
                    return Err(InvalidFrame(
                        "camera raw image length does not match dimensions",
                    ));
                }
            }
            Encoding::Jpeg | Encoding::Png if data.is_empty() => {
                return Err(InvalidFrame("camera encoded image must not be empty"));
            }
            Encoding::Jpeg | Encoding::Png => {}
        }
        if let Some(value) = intrinsics
            && !(value.fx.is_finite()
                && value.fy.is_finite()
                && value.fx > 0.0
                && value.fy > 0.0
                && value.cx.is_finite()
                && value.cy.is_finite())
        {
            return Err(InvalidFrame(
                "camera intrinsics must be finite with positive focal lengths",
            ));
        }
        if let Some(value) = &distortion
            && (value.model.trim().is_empty() || !value.coefficients.iter().all(|v| v.is_finite()))
        {
            return Err(InvalidFrame("camera distortion must be named and finite"));
        }
        if let Some(value) = &calibration
            && (value.id.trim().is_empty() || value.version.trim().is_empty())
        {
            return Err(InvalidFrame("camera calibration identity must be nonempty"));
        }
        Ok(Self {
            width,
            height,
            encoding,
            intrinsics,
            distortion,
            exposure,
            calibration,
            data,
        })
    }
}

impl TryFrom<FrameWire> for Frame {
    type Error = InvalidFrame;
    fn try_from(value: FrameWire) -> Result<Self, Self::Error> {
        Self::try_new(
            value.width,
            value.height,
            value.encoding,
            value.intrinsics,
            value.distortion,
            value.exposure,
            value.calibration,
            value.data,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constructor_checks_raw_shape() {
        assert!(Frame::try_new(2, 2, Encoding::Rgb8, None, None, None, None, vec![0; 3]).is_err());
    }
}
