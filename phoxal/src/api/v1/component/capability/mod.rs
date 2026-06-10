pub mod accelerometer;
pub mod battery;
pub mod camera;
pub mod depth;
pub mod emergency_stop;
pub mod encoder;
pub mod gnss;
pub mod gyroscope;
pub mod imu;
pub mod led;
pub mod lidar;
pub mod magnetometer;
pub mod microphone;
pub mod mmwave;
pub mod motor;
pub mod profile;
pub mod range;
pub mod speaker;

pub const DATA_STREAM: &str = "data";
pub const COMMAND_STREAM: &str = "command";
pub const HEALTH_STREAM: &str = "health";
pub const PROFILE_STREAM: &str = "profile";

pub fn stream_path(
    component_id: impl AsRef<str>,
    capability_kind: &str,
    capability_id: impl AsRef<str>,
    stream: &str,
) -> String {
    format!(
        "component/{}/{}/{}/{}",
        component_id.as_ref(),
        capability_kind,
        capability_id.as_ref(),
        stream
    )
}

pub fn data_path(
    component_id: impl AsRef<str>,
    capability_kind: &str,
    capability_id: impl AsRef<str>,
) -> String {
    stream_path(component_id, capability_kind, capability_id, DATA_STREAM)
}

pub fn command_path(
    component_id: impl AsRef<str>,
    capability_kind: &str,
    capability_id: impl AsRef<str>,
) -> String {
    stream_path(component_id, capability_kind, capability_id, COMMAND_STREAM)
}

pub fn health_path(
    component_id: impl AsRef<str>,
    capability_kind: &str,
    capability_id: impl AsRef<str>,
) -> String {
    stream_path(component_id, capability_kind, capability_id, HEALTH_STREAM)
}

pub fn profile_path(
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
    profile_id: &profile::ProfileId,
) -> String {
    format!(
        "component/{}/{}/{}/{}",
        component_id.as_ref(),
        capability_id.as_ref(),
        PROFILE_STREAM,
        profile_id.as_ref()
    )
}

pub fn default_profile_path(
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> String {
    profile_path(
        component_id,
        capability_id,
        &profile::ProfileId::default_profile(),
    )
}
