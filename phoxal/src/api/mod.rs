//! The single API layer (D60/D61).
//!
//! API versions are **dated modules** (`phoxal::api::y2026_1`, …). Each carries a
//! zero-variant marker `enum Api {}` implementing [`ApiVersion`], the
//! version-local wire bodies (plain serde structs/enums — the wire payload has no
//! `{"v":…}` version tag, D62), their [`ContractBody`] impls, and an api-local
//! `topic` builder (`api::topic::new().drive().state()`).
//!
//! A runtime authors against exactly one of these modules
//! (`use phoxal::api::y2026_1 as api;`) and declares it on the derive
//! (`#[phoxal(api = y2026_1)]`); every handle body is bound
//! `ContractBody<Api = R::Api>`, so a body from another API version is a compile
//! error (D59/D60).

use phoxal_macros::phoxal_api_tree;

/// Marker trait identifying one dated API version (D60). The `ID` is the dated
/// module name (`"y2026_1"`); it is the canonical version identity, carried in
/// bus metadata — never in the wire body or the topic key (D62).
pub trait ApiVersion: 'static {
    /// The dated API-version identifier, e.g. `"y2026_1"`.
    const ID: &'static str;
}

/// A version-local wire body: a plain serde type bound to exactly one
/// [`ApiVersion`] and one contract family/topic (D61).
///
/// The macro-generated bodies impl this. Handles, `SetupContext` builders, and
/// the `#[derive(Runtime)]` assertions all key off `Api`/`FAMILY`/`TOPIC`.
pub trait ContractBody:
    serde::Serialize + serde::de::DeserializeOwned + Clone + Send + Sync + 'static
{
    /// The one API version this body belongs to.
    type Api: ApiVersion;
    /// Canonical contract family id, e.g. `"drive::State"`.
    const FAMILY: &'static str;
    /// Versionless topic key, e.g. `"drive/state"`.
    const TOPIC: &'static str;
}

phoxal_api_tree! {
    version y2026_1 {
        drive {
            /// Why actuation authority is in its current state.
            enum StopReason {
                NoTarget,
                EmergencyStop,
                Fault,
            }

            /// Whether the drive is actively commanding the actuators.
            enum ActuatorAuthority {
                Active,
                Stopped,
            }

            /// A requested or limited planar velocity.
            struct Target {
                linear_x_mps: f32,
                angular_z_radps: f32,
                curvature_limit_radpm: Option<f32>,
            }

            /// The drive runtime's published control state.
            struct State {
                target: Target,
                limited_target: Target,
                actuator_authority: ActuatorAuthority,
                stop_reason: Option<StopReason>,
            }

            topic target: pubsub Target;
            topic state: pubsub State;
        }

        battery {
            /// Battery state — a family that exists only from y2026_1 on.
            struct State {
                voltage_v: f32,
                current_a: f32,
                charge_ratio: f32,
            }

            topic state: pubsub State;
        }

        safety {
            /// The robot's overall safety posture, worst-concern-wins.
            enum Level {
                /// No active concerns.
                Nominal,
                /// A degraded condition that does not (yet) require stopping.
                Warning,
                /// Actuation authority must be revoked immediately.
                EmergencyStop,
            }

            /// A specific reason the safety monitor raised the posture.
            enum Concern {
                BatteryLow,
                BatteryCritical,
                DriveFault,
            }

            /// The published safety posture: the worst level across all active
            /// concerns, plus the concerns that drove it.
            struct Status {
                level: Level,
                concerns: Vec<Concern>,
            }

            topic state: pubsub Status;
        }

        motor {
            /// A per-actuator command.
            enum Command {
                Velocity(f32),
                Torque(f32),
                Stop,
            }

            topic command: pubsub Command;
        }

        component {
            /// A command to a specific component-instance capability, addressed by
            /// a dynamic per-instance key (D17/D38: framework-runtime / driver
            /// territory). The runtime fills `{instance}`/`{capability}` from its
            /// manifest-declared component bindings.
            enum MotorCommand {
                Velocity(f32),
                Torque(f32),
                Stop,
            }

            /// Per-encoder sample on a dynamic per-instance key.
            struct EncoderSample {
                position_rad: f64,
                velocity_radps: f32,
            }

            /// Raw accelerometer sample in the sensor-local frame in m/s^2.
            struct AccelerometerSample {
                linear_acceleration: [f32; 3],
            }

            /// Raw angular velocity sample in the sensor-local frame in rad/s.
            struct GyroscopeSample {
                angular_velocity: [f32; 3],
            }

            struct MagnetometerSample {
                magnetic_field: [f32; 3],
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            enum SensorHealth {
                Nominal,
                Degraded,
                Fault,
            }

            #[derive(Copy)]
            struct Bias {
                angular_velocity_radps: [f32; 3],
                linear_acceleration_mps2: [f32; 3],
            }

            struct ImuSample {
                orientation: Option<[f32; 4]>,
                angular_velocity_radps: [f32; 3],
                linear_acceleration_mps2: [f32; 3],
                covariance: Option<[f32; 9]>,
                noise_density: Option<[f32; 3]>,
                sensor_frame_id: Option<String>,
                measured_at_ns: Option<u64>,
                health: SensorHealth,
                bias: Option<Bias>,
            }

            #[derive(Copy)]
            struct Limits {
                min_m: f32,
                max_m: f32,
            }

            #[derive(Copy)]
            struct SampleQuality {
                valid: bool,
                confidence: Option<f32>,
            }

            struct RangeSample {
                distance_m: f32,
                limits: Option<Limits>,
                measured_at_ns: Option<u64>,
                quality: Option<SampleQuality>,
                health: SensorHealth,
            }

            #[derive(Default, Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            enum CoordinateSystem {
                #[default]
                Local,
                Wgs84,
            }

            struct GnssSample {
                latitude: f64,
                longitude: f64,
                altitude: f64,
                position_covariance: [f64; 9],
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            enum CameraEncoding {
                Jpeg,
                Png,
                L8,
                Rgb8,
                Rgba8,
            }

            #[derive(Copy)]
            struct CameraIntrinsics {
                fx: f32,
                fy: f32,
                cx: f32,
                cy: f32,
            }

            struct CameraDistortion {
                model: String,
                coefficients: Vec<f32>,
            }

            #[derive(Copy)]
            struct CameraExposureTiming {
                exposure_start_ns: Option<u64>,
                exposure_duration_ns: Option<u64>,
            }

            struct CameraCalibrationIdentity {
                id: String,
                version: String,
            }

            struct CameraFrame {
                width: u32,
                height: u32,
                encoding: CameraEncoding,
                intrinsics: Option<CameraIntrinsics>,
                distortion: Option<CameraDistortion>,
                exposure: Option<CameraExposureTiming>,
                measured_at_ns: Option<u64>,
                calibration: Option<CameraCalibrationIdentity>,
                #[serde(with = "serde_bytes")]
                data: Vec<u8>,
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            enum DepthEncoding {
                U16Millimeters,
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            enum DepthInvalidSamplePolicy {
                ZeroIsInvalid,
                NonFiniteIsInvalid,
            }

            struct DepthFrame {
                samples_mm: Vec<u16>,
                encoding: DepthEncoding,
                invalid_sample_policy: DepthInvalidSamplePolicy,
                width: Option<u32>,
                height: Option<u32>,
                intrinsics: Option<CameraIntrinsics>,
                distortion: Option<CameraDistortion>,
                exposure: Option<CameraExposureTiming>,
                measured_at_ns: Option<u64>,
                calibration: Option<CameraCalibrationIdentity>,
            }

            #[serde(tag = "kind", rename_all = "snake_case")]
            enum LidarScan {
                Ranges(LidarRanges),
                Points(LidarPoints),
            }

            struct LidarRanges {
                ranges: Vec<f32>,
                geometry: Option<LidarScanGeometry>,
                limits: Option<LidarRangeLimits>,
                measured_at_ns: Option<u64>,
                quality: Option<LidarScanQuality>,
                health: SensorHealth,
            }

            struct LidarPoints {
                points: Vec<[f32; 3]>,
                limits: Option<LidarRangeLimits>,
                measured_at_ns: Option<u64>,
                quality: Option<LidarScanQuality>,
                health: SensorHealth,
            }

            #[derive(Copy)]
            struct LidarScanGeometry {
                angle_min_rad: f32,
                angle_increment_rad: f32,
            }

            #[derive(Copy)]
            struct LidarRangeLimits {
                min_m: f32,
                max_m: f32,
            }

            #[derive(Copy)]
            struct LidarScanQuality {
                valid_points: u32,
            }

            struct MmwaveScan {
                detections: Vec<MmwaveDetection>,
            }

            #[derive(Copy)]
            struct MmwaveDetection {
                position: [f32; 3],
                velocity: [f32; 3],
                snr: f32,
            }

            struct MicrophoneFrame {
                data: Vec<u8>,
            }

            #[derive(Copy, Eq)]
            enum LedCommand {
                On,
                Off,
            }

            #[derive(Eq)]
            struct EmergencyStopState {
                engaged: bool,
            }

            #[derive(Eq, PartialOrd, Ord, Hash)]
            struct ProfileId(pub String);

            struct ImageCrop {
                origin_x_px: u32,
                origin_y_px: u32,
                width_px: u32,
                height_px: u32,
            }

            struct AngularRoi {
                start_rad: f32,
                end_rad: f32,
            }

            #[derive(Eq)]
            #[serde(rename_all = "snake_case")]
            enum CameraProfileEncoding {
                L8,
                Rgb8,
                Rgba8,
                Jpeg,
                Png,
            }

            #[derive(Eq)]
            #[serde(rename_all = "snake_case")]
            enum DepthProfileEncoding {
                U16Millimeters,
            }

            struct CameraProfileSpec {
                width_px: u32,
                height_px: u32,
                publish_rate_hz: f64,
                encoding: CameraProfileEncoding,
            }

            enum ParsedCameraProfileSpec {
                Native,
                Spec(CameraProfileSpec),
            }

            struct DepthProfileSpec {
                width_px: u32,
                height_px: u32,
                publish_rate_hz: f64,
            }

            enum ParsedDepthProfileSpec {
                Native,
                Spec(DepthProfileSpec),
            }

            struct RateProfileSpec {
                publish_rate_hz: f64,
            }

            enum ParsedRateProfileSpec {
                Native,
                Spec(RateProfileSpec),
            }

            topic motor_command(instance, capability): pubsub MotorCommand
                = "component/{instance}/motor/{capability}/command";
            topic encoder_sample(instance, capability): pubsub EncoderSample
                = "component/{instance}/encoder/{capability}/sample";
            topic accelerometer_sample(instance, capability): pubsub AccelerometerSample
                = "component/{instance}/accelerometer/{capability}/sample";
            topic gyroscope_sample(instance, capability): pubsub GyroscopeSample
                = "component/{instance}/gyroscope/{capability}/sample";
            topic magnetometer_sample(instance, capability): pubsub MagnetometerSample
                = "component/{instance}/magnetometer/{capability}/sample";
            topic imu_sample(instance, capability): pubsub ImuSample
                = "component/{instance}/imu/{capability}/sample";
            topic range_sample(instance, capability): pubsub RangeSample
                = "component/{instance}/range/{capability}/sample";
            topic gnss_sample(instance, capability): pubsub GnssSample
                = "component/{instance}/gnss/{capability}/sample";
            topic camera_frame(instance, capability): pubsub CameraFrame
                = "component/{instance}/camera/{capability}/frame";
            topic depth_frame(instance, capability): pubsub DepthFrame
                = "component/{instance}/depth/{capability}/frame";
            topic lidar_scan(instance, capability): pubsub LidarScan
                = "component/{instance}/lidar/{capability}/scan";
            topic mmwave_scan(instance, capability): pubsub MmwaveScan
                = "component/{instance}/mmwave/{capability}/scan";
            topic microphone_frame(instance, capability): pubsub MicrophoneFrame
                = "component/{instance}/microphone/{capability}/frame";
            topic led_command(instance, capability): pubsub LedCommand
                = "component/{instance}/led/{capability}/command";
            topic emergency_stop_state(instance, capability): pubsub EmergencyStopState
                = "component/{instance}/emergency_stop/{capability}/state";
        }

        odometry {
            /// A planar pose + twist estimate in the odometry frame.
            struct State {
                x_m: f64,
                y_m: f64,
                yaw_rad: f64,
                linear_x_mps: f32,
                angular_z_radps: f32,
            }

            topic state: pubsub State;
        }

        localize {
            /// A planar localization estimate in the map frame.
            struct LocalizationState {
                x_m: f64,
                y_m: f64,
                yaw_rad: f64,
                confidence: f32,
            }

            topic state: pubsub LocalizationState;
        }

        presence {
            /// Per-participant liveness + readiness beacon.
            enum Readiness {
                NotStarted,
                Initializing,
                Ready,
                Degraded,
                Failed,
            }

            struct Heartbeat {
                participant: String,
                readiness: Readiness,
            }

            topic heartbeat: pubsub Heartbeat;
        }

        map {
            /// A published map revision marker.
            struct Revision {
                revision: u64,
                resolution_m: f32,
            }

            /// Request a rectangular submap window (map-frame metres).
            struct SubmapRequest {
                min_x_m: f64,
                min_y_m: f64,
                max_x_m: f64,
                max_y_m: f64,
            }

            /// An occupancy-grid window: row-major cells, 0..=100 + 255 = unknown.
            struct SubmapResponse {
                width: u32,
                height: u32,
                resolution_m: f32,
                cells: Vec<u8>,
            }

            topic revision: pubsub Revision;
            topic submap: query SubmapRequest => SubmapResponse;
        }

        asset {
            /// Fetch a stored asset by path.
            struct GetRequest {
                path: String,
            }

            /// The asset bytes, a not-found marker, or a rejected path.
            enum GetResponse {
                Found { bytes: Vec<u8> },
                Missing,
                InvalidPath,
            }

            topic get: query GetRequest => GetResponse;
        }
    }
}

#[cfg(test)]
mod tests;
