#![allow(clippy::module_name_repetitions)]

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Profile identifier, e.g. "r640x480_h15_rgb8" or the reserved native id.
/// Validated syntax (see ProfileId::new / TryFrom<String>).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProfileId(String);

impl ProfileId {
    pub const DEFAULT: &str = "default";

    pub fn new(value: impl Into<String>) -> Result<Self, InvalidProfileIdentifier> {
        let value = value.into();
        validate_profile_identifier(&value)?;
        Ok(Self(value))
    }

    /// The reserved implicit "default" profile id (the capability's native stream).
    pub fn default_profile() -> Self {
        Self(Self::DEFAULT.to_string())
    }
}

impl AsRef<str> for ProfileId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ProfileId {
    type Err = InvalidProfileIdentifier;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ProfileId {
    type Error = InvalidProfileIdentifier;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ProfileId> for String {
    fn from(value: ProfileId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InvalidProfileIdentifier {
    value: String,
}

impl fmt::Display for InvalidProfileIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid profile identifier '{}', expected a non-empty non-reserved lowercase ASCII profile id using letters, digits, '.', '_' or '-' with no leading, trailing, or repeated '.'",
            self.value
        )
    }
}

impl std::error::Error for InvalidProfileIdentifier {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageCrop {
    pub origin_x_px: u32,
    pub origin_y_px: u32,
    pub width_px: u32,
    pub height_px: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AngularRoi {
    pub start_rad: f32,
    pub end_rad: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraProfileEncoding {
    L8,
    Rgb8,
    Rgba8,
    Jpeg,
    Png,
}

impl CameraProfileEncoding {
    #[must_use]
    pub const fn profile_token(&self) -> &'static str {
        match self {
            Self::L8 => "l8",
            Self::Rgb8 => "rgb8",
            Self::Rgba8 => "rgba8",
            Self::Jpeg => "jpeg",
            Self::Png => "png",
        }
    }

    fn from_profile_token(value: &str) -> Option<Self> {
        match value {
            "l8" => Some(Self::L8),
            "rgb8" => Some(Self::Rgb8),
            "rgba8" => Some(Self::Rgba8),
            "jpeg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DepthProfileEncoding {
    U16Millimeters,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraProfileSpec {
    pub width_px: u32,
    pub height_px: u32,
    pub publish_rate_hz: f64,
    pub encoding: CameraProfileEncoding,
}

impl CameraProfileSpec {
    pub fn to_profile_id(&self) -> Result<ProfileId, ProfileSpecError> {
        validate_spec_dimensions(self.width_px, self.height_px)?;
        let rate_hz = integer_rate_hz(self.publish_rate_hz)?;
        ProfileId::new(format!(
            "r{}x{}_h{}_{}",
            self.width_px,
            self.height_px,
            rate_hz,
            self.encoding.profile_token()
        ))
        .map_err(ProfileSpecError::InvalidIdentifier)
    }

    pub fn from_profile_id(
        profile_id: &ProfileId,
    ) -> Result<ParsedCameraProfileSpec, ProfileSpecError> {
        let value = profile_id.as_ref();
        if value == ProfileId::DEFAULT {
            return Ok(ParsedCameraProfileSpec::Native);
        }

        let (width_px, height_px, publish_rate_hz, encoding_token) =
            parse_profile_spec_parts(value)?;
        let Some(encoding) = CameraProfileEncoding::from_profile_token(encoding_token) else {
            return Err(ProfileSpecError::InvalidFormat {
                value: value.to_string(),
            });
        };

        Ok(ParsedCameraProfileSpec::Spec(Self {
            width_px,
            height_px,
            publish_rate_hz,
            encoding,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParsedCameraProfileSpec {
    Native,
    Spec(CameraProfileSpec),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DepthProfileSpec {
    pub width_px: u32,
    pub height_px: u32,
    pub publish_rate_hz: f64,
}

impl DepthProfileSpec {
    pub fn to_profile_id(&self) -> Result<ProfileId, ProfileSpecError> {
        validate_spec_dimensions(self.width_px, self.height_px)?;
        let rate_hz = integer_rate_hz(self.publish_rate_hz)?;
        ProfileId::new(format!(
            "r{}x{}_h{}",
            self.width_px, self.height_px, rate_hz
        ))
        .map_err(ProfileSpecError::InvalidIdentifier)
    }

    pub fn from_profile_id(
        profile_id: &ProfileId,
    ) -> Result<ParsedDepthProfileSpec, ProfileSpecError> {
        let value = profile_id.as_ref();
        if value == ProfileId::DEFAULT {
            return Ok(ParsedDepthProfileSpec::Native);
        }

        let (width_px, height_px, publish_rate_hz, encoding_token) =
            parse_profile_spec_parts(value)?;
        if !encoding_token.is_empty() {
            return Err(ProfileSpecError::InvalidFormat {
                value: value.to_string(),
            });
        }

        Ok(ParsedDepthProfileSpec::Spec(Self {
            width_px,
            height_px,
            publish_rate_hz,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParsedDepthProfileSpec {
    Native,
    Spec(DepthProfileSpec),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateProfileSpec {
    pub publish_rate_hz: f64,
}

impl RateProfileSpec {
    pub fn to_profile_id(&self) -> Result<ProfileId, ProfileSpecError> {
        let rate_hz = integer_rate_hz(self.publish_rate_hz)?;
        ProfileId::new(format!("h{rate_hz}")).map_err(ProfileSpecError::InvalidIdentifier)
    }

    pub fn from_profile_id(
        profile_id: &ProfileId,
    ) -> Result<ParsedRateProfileSpec, ProfileSpecError> {
        let value = profile_id.as_ref();
        if value == ProfileId::DEFAULT {
            return Ok(ParsedRateProfileSpec::Native);
        }

        Ok(ParsedRateProfileSpec::Spec(Self {
            publish_rate_hz: parse_rate_profile_spec(value)?,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParsedRateProfileSpec {
    Native,
    Spec(RateProfileSpec),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProfileSpecError {
    InvalidIdentifier(InvalidProfileIdentifier),
    InvalidFormat { value: String },
    InvalidDimensions { width_px: u32, height_px: u32 },
    InvalidPublishRate { publish_rate_hz: f64 },
}

impl fmt::Display for ProfileSpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentifier(error) => error.fmt(f),
            Self::InvalidFormat { value } => write!(
                f,
                "invalid profile spec '{}', expected camera form r{{w}}x{{h}}_h{{rate}}_{{enc}}, depth form r{{w}}x{{h}}_h{{rate}}, or rate form h{{rate}}",
                value
            ),
            Self::InvalidDimensions {
                width_px,
                height_px,
            } => write!(
                f,
                "invalid profile spec dimensions {}x{}, width and height must be > 0",
                width_px, height_px
            ),
            Self::InvalidPublishRate { publish_rate_hz } => write!(
                f,
                "invalid profile spec publish_rate_hz {}, expected a positive integer Hz value",
                publish_rate_hz
            ),
        }
    }
}

impl std::error::Error for ProfileSpecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidIdentifier(error) => Some(error),
            Self::InvalidFormat { .. }
            | Self::InvalidDimensions { .. }
            | Self::InvalidPublishRate { .. } => None,
        }
    }
}

fn validate_profile_identifier(value: &str) -> Result<(), InvalidProfileIdentifier> {
    let valid = !value.is_empty()
        && value != "default"
        && !value.starts_with('.')
        && !value.ends_with('.')
        && !value.contains("..")
        && value.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '.'
                || character == '_'
                || character == '-'
        });

    if valid {
        Ok(())
    } else {
        Err(InvalidProfileIdentifier {
            value: value.to_string(),
        })
    }
}

fn validate_spec_dimensions(width_px: u32, height_px: u32) -> Result<(), ProfileSpecError> {
    if width_px == 0 || height_px == 0 {
        Err(ProfileSpecError::InvalidDimensions {
            width_px,
            height_px,
        })
    } else {
        Ok(())
    }
}

fn integer_rate_hz(publish_rate_hz: f64) -> Result<u64, ProfileSpecError> {
    if !publish_rate_hz.is_finite() || publish_rate_hz <= 0.0 || publish_rate_hz.fract() != 0.0 {
        return Err(ProfileSpecError::InvalidPublishRate { publish_rate_hz });
    }
    Ok(publish_rate_hz as u64)
}

fn parse_profile_spec_parts(value: &str) -> Result<(u32, u32, f64, &str), ProfileSpecError> {
    let Some(after_resolution_marker) = value.strip_prefix('r') else {
        return Err(ProfileSpecError::InvalidFormat {
            value: value.to_string(),
        });
    };
    let Some((resolution, after_rate_marker)) = after_resolution_marker.split_once("_h") else {
        return Err(ProfileSpecError::InvalidFormat {
            value: value.to_string(),
        });
    };
    let Some((width, height)) = resolution.split_once('x') else {
        return Err(ProfileSpecError::InvalidFormat {
            value: value.to_string(),
        });
    };
    let (rate, encoding_token) = after_rate_marker
        .split_once('_')
        .unwrap_or((after_rate_marker, ""));
    if width.is_empty() || height.is_empty() || rate.is_empty() {
        return Err(ProfileSpecError::InvalidFormat {
            value: value.to_string(),
        });
    }

    let width_px = width
        .parse::<u32>()
        .map_err(|_| ProfileSpecError::InvalidFormat {
            value: value.to_string(),
        })?;
    let height_px = height
        .parse::<u32>()
        .map_err(|_| ProfileSpecError::InvalidFormat {
            value: value.to_string(),
        })?;
    let rate_hz = rate
        .parse::<u64>()
        .map_err(|_| ProfileSpecError::InvalidFormat {
            value: value.to_string(),
        })?;
    validate_spec_dimensions(width_px, height_px)?;
    if rate_hz == 0 {
        return Err(ProfileSpecError::InvalidPublishRate {
            publish_rate_hz: 0.0,
        });
    }

    Ok((width_px, height_px, rate_hz as f64, encoding_token))
}

fn parse_rate_profile_spec(value: &str) -> Result<f64, ProfileSpecError> {
    let Some(rate) = value.strip_prefix('h') else {
        return Err(ProfileSpecError::InvalidFormat {
            value: value.to_string(),
        });
    };
    if rate.is_empty() {
        return Err(ProfileSpecError::InvalidFormat {
            value: value.to_string(),
        });
    }

    let rate_hz = rate
        .parse::<u64>()
        .map_err(|_| ProfileSpecError::InvalidFormat {
            value: value.to_string(),
        })?;
    if rate_hz == 0 {
        return Err(ProfileSpecError::InvalidPublishRate {
            publish_rate_hz: 0.0,
        });
    }

    Ok(rate_hz as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_path_uses_source_and_profile_without_kind_or_data_suffix() {
        let profile_id = ProfileId::new("localize.rgbd_color").unwrap();

        assert_eq!(
            super::super::profile_path("front_camera", "rgb", &profile_id),
            "component/front_camera/rgb/profile/localize.rgbd_color"
        );
    }

    #[test]
    fn default_profile_uses_reserved_implicit_profile_path() {
        assert_eq!(
            super::super::profile_path("c", "cap", &ProfileId::default_profile()),
            "component/c/cap/profile/default"
        );
        assert!(ProfileId::new("default").is_err());
    }

    #[test]
    fn profile_id_accepts_topic_segment_shape() {
        for value in [
            "localize.rgbd_color",
            "operator.preview",
            "native",
            "rerun.debug_preview",
        ] {
            assert_eq!(ProfileId::new(value).unwrap().as_ref(), value);
        }
    }

    #[test]
    fn profile_id_rejects_reserved_default() {
        assert!(ProfileId::new("default").is_err());
    }

    #[test]
    fn profile_id_rejects_invalid_topic_segment_shape() {
        for value in [
            "",
            "Foo",
            "a..b",
            ".lead",
            "trail.",
            "has space",
            "has/slash",
        ] {
            assert!(
                ProfileId::new(value).is_err(),
                "expected '{value}' to be rejected"
            );
        }
    }

    #[test]
    fn camera_profile_spec_round_trips_through_profile_id() {
        let spec = CameraProfileSpec {
            width_px: 640,
            height_px: 480,
            publish_rate_hz: 15.0,
            encoding: CameraProfileEncoding::Rgb8,
        };

        let profile_id = spec.to_profile_id().unwrap();
        let parsed = CameraProfileSpec::from_profile_id(&profile_id).unwrap();

        assert_eq!(profile_id.as_ref(), "r640x480_h15_rgb8");
        assert_eq!(parsed, ParsedCameraProfileSpec::Spec(spec));
    }

    #[test]
    fn depth_profile_spec_round_trips_through_profile_id() {
        let spec = DepthProfileSpec {
            width_px: 320,
            height_px: 240,
            publish_rate_hz: 5.0,
        };

        let profile_id = spec.to_profile_id().unwrap();
        let parsed = DepthProfileSpec::from_profile_id(&profile_id).unwrap();

        assert_eq!(profile_id.as_ref(), "r320x240_h5");
        assert_eq!(parsed, ParsedDepthProfileSpec::Spec(spec));
    }

    #[test]
    fn rate_profile_spec_round_trips_through_profile_id() {
        let spec = RateProfileSpec {
            publish_rate_hz: 50.0,
        };

        let profile_id = spec.to_profile_id().unwrap();
        let parsed = RateProfileSpec::from_profile_id(&profile_id).unwrap();

        assert_eq!(profile_id.as_ref(), "h50");
        assert_eq!(parsed, ParsedRateProfileSpec::Spec(spec));
    }

    #[test]
    fn default_profile_parses_as_native_sentinel() {
        assert_eq!(
            CameraProfileSpec::from_profile_id(&ProfileId::default_profile()).unwrap(),
            ParsedCameraProfileSpec::Native
        );
        assert_eq!(
            DepthProfileSpec::from_profile_id(&ProfileId::default_profile()).unwrap(),
            ParsedDepthProfileSpec::Native
        );
        assert_eq!(
            RateProfileSpec::from_profile_id(&ProfileId::default_profile()).unwrap(),
            ParsedRateProfileSpec::Native
        );
    }

    #[test]
    fn out_of_form_profile_id_errors() {
        let profile_id = ProfileId::new("localize.rgb").unwrap();

        assert!(CameraProfileSpec::from_profile_id(&profile_id).is_err());
        assert!(DepthProfileSpec::from_profile_id(&profile_id).is_err());
        assert!(RateProfileSpec::from_profile_id(&profile_id).is_err());
    }

    #[test]
    fn malformed_rate_profile_ids_error() {
        for value in ["h", "h0", "h5.5", "h50_extra", "r320x240_h5"] {
            let profile_id = ProfileId::new(value).unwrap();
            assert!(
                RateProfileSpec::from_profile_id(&profile_id).is_err(),
                "expected '{value}' to be rejected"
            );
        }
    }
}
