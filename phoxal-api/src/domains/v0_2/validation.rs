//! Shared validation for the current robot wire domains.

pub(crate) const MAX_FRAME_ID_LEN: usize = 128;
pub(crate) const MAX_REQUEST_ID_LEN: usize = 128;
pub(crate) const MAX_PATH_POSES: usize = 4096;

pub(crate) fn valid_frame_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_FRAME_ID_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

pub(crate) fn valid_request_id(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.len() <= MAX_REQUEST_ID_LEN
        && trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(crate) fn finite(value: f64) -> bool {
    value.is_finite()
}

pub(crate) fn finite_f32(value: f32) -> bool {
    value.is_finite()
}

pub(crate) fn canonical_yaw(value: f64) -> bool {
    value.is_finite() && (-std::f64::consts::PI..=std::f64::consts::PI).contains(&value)
}

pub(crate) fn optional_canonical_yaw(value: Option<f64>) -> bool {
    value.is_none_or(canonical_yaw)
}
