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
            /// Safety runtime decision for a candidate motion command.
            #[derive(Copy, Eq)]
            enum SafetyDecision {
                Allow,
                Slow,
                Stop,
                EmergencyStop,
                UnknownConservative,
            }

            struct Constraint {
                min: f64,
                max: f64,
            }

            struct MotionConstraint {
                linear_x_mps: Constraint,
                angular_z_radps: Constraint,
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            enum SafetyReasonCode {
                ObstacleDetected,
                BatteryLow,
                BatteryCritical,
                DriveFault,
                LocalizationLost,
                SourceStale,
                EmergencyStopEngaged,
                Unknown,
            }

            struct SafetyReason {
                code: SafetyReasonCode,
                detail: Option<String>,
            }

            struct SafetySourceRevision {
                localization: Option<u64>,
                map: Option<u64>,
            }

            struct SafetyAuthorization {
                decision: SafetyDecision,
                approved_motion: MotionConstraint,
                reasons: Vec<SafetyReason>,
                source_revision: SafetySourceRevision,
                expires_at_ns: Option<u64>,
            }

            struct Status {
                decision: SafetyDecision,
                active_reasons: Vec<SafetyReason>,
            }

            #[derive(Eq)]
            struct EmergencyStopRequest {
                engaged: bool,
            }

            topic authorization: pubsub SafetyAuthorization;
            topic state: pubsub Status;
            topic estop: pubsub EmergencyStopRequest;
        }

        mission {
            struct Goal {
                x_m: f64,
                y_m: f64,
                yaw_rad: Option<f64>,
            }

            enum Command {
                Start(Goal),
                Pause,
                Resume,
                Cancel,
            }

            #[derive(Copy, Eq)]
            enum Phase {
                Idle,
                Active,
                Paused,
                Succeeded,
                Failed,
            }

            struct State {
                phase: Phase,
                goal: Option<Goal>,
                detail: Option<String>,
            }

            topic command: pubsub Command;
            topic goal: pubsub Goal;
            topic state: pubsub State;
        }

        joint(joint) {
            struct JointState {
                position_rad: f64,
                velocity_radps: f64,
                effort_nm: Option<f64>,
            }

            topic state: pubsub JointState;
        }

        frame {
            struct FrameTransform {
                parent_frame_id: String,
                child_frame_id: String,
                translation_m: [f64; 3],
                rotation_quat_xyzw: [f64; 4],
                stamp_ns: Option<u64>,
            }

            struct StaticTransforms {
                transforms: Vec<FrameTransform>,
            }

            struct Tree {
                transforms: Vec<FrameTransform>,
            }

            struct LookupRequest {
                target_frame_id: String,
                source_frame_id: String,
                at_ns: Option<u64>,
            }

            struct LookupResponse {
                transform: Option<FrameTransform>,
            }

            topic tree: pubsub Tree;
            topic static_transforms: pubsub StaticTransforms;
            topic lookup: query LookupRequest => LookupResponse;
        }

        power {
            #[derive(Copy, Eq)]
            enum Command {
                Reboot,
                Shutdown,
            }

            #[derive(Copy, Eq)]
            enum Status {
                Idle,
                Rebooting,
                ShuttingDown,
                Failed,
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            enum RejectedReason {
                SupervisorUnavailable,
                SupervisorReturnedHttp,
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            enum FailedReason {
                SupervisorTransport,
            }

            struct State {
                status: Status,
                detail: Option<String>,
            }

            topic command: pubsub Command;
            topic state: pubsub State;
        }

        motion {
            struct Target {
                linear_x_mps: f32,
                angular_z_radps: f32,
                curvature_limit_radpm: Option<f32>,
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            enum SafetyDecision {
                Allow,
                Slow,
                Stop,
                EmergencyStop,
                UnknownConservative,
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            enum MotionSource {
                Manual,
                Follow,
                MissionStop,
                Recovery,
                EmergencyStop,
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            enum MotionReason {
                SafetyEmergencyStop,
                ManualEscapeUnderStop,
                SafetyConstrained(SafetyDecision),
                NoFollowTarget,
                FollowTargetStale,
                SafetyAuthorizationUnavailable,
            }

            struct ManualCommand {
                linear_x_mps: f64,
                angular_z_radps: f64,
            }

            struct State {
                active_source: Option<MotionSource>,
                selected: Option<Target>,
                reason: Option<MotionReason>,
            }

            topic manual: pubsub ManualCommand;
            topic state: pubsub State;
        }

        plan {
            struct PathPose {
                x_m: f64,
                y_m: f64,
                yaw_rad: Option<f64>,
            }

            struct Path {
                poses: Vec<PathPose>,
                map_revision: Option<u64>,
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            enum Refusal {
                MissionInactive,
                NoGoal,
                NoMap,
                NoLocalization,
                Unreachable,
                NonPlanarGoalUnsupported,
                LocalizationInitializing,
                LocalizationLost,
                LocalizationRelocalizing,
                UnsupportedLocalizationMode,
                NoLocalizationPose,
                NoLocalizationRevision,
                GoalMapRevisionMismatch,
                MapLocalizeRevisionMismatch,
            }

            struct State {
                has_path: bool,
                refusal: Option<Refusal>,
            }

            topic path: pubsub Path;
            topic state: pubsub State;
        }

        follow {
            struct Target {
                map_revision: Option<u64>,
                built_from_localize_revision: Option<u64>,
                frame_id: String,
                linear_x_mps: f64,
                angular_z_radps: f64,
            }

            struct State {
                active: bool,
                target_index: Option<u32>,
                finished: bool,
            }

            topic target: pubsub Target;
            topic state: pubsub State;
        }

        explore {
            struct Frontier {
                x_m: f64,
                y_m: f64,
                size: u32,
                score: f32,
            }

            struct Frontiers {
                frontiers: Vec<Frontier>,
                map_revision: Option<u64>,
            }

            struct State {
                exploring: bool,
                selected: Option<Frontier>,
            }

            topic frontiers: pubsub Frontiers;
            topic state: pubsub State;
        }

        perception {
            struct Detection {
                class_id: String,
                confidence: f32,
                position_m: [f64; 3],
                frame_id: String,
                track_id: Option<u64>,
            }

            struct Detections {
                detections: Vec<Detection>,
                stamp_ns: Option<u64>,
            }

            struct State {
                healthy: bool,
                detector: String,
            }

            topic detections: pubsub Detections;
            topic state: pubsub State;
        }

        video {
            struct OpenRequest {
                capability: String,
                width_px: Option<u32>,
                height_px: Option<u32>,
            }

            struct OpenResponse {
                stream_id: String,
            }

            topic open: query OpenRequest => OpenResponse;

            stream(stream) {
                #[derive(Copy, Eq)]
                enum StreamEvent {
                    Started,
                    KeyFrame,
                    Stopped,
                }

                topic event: pubsub StreamEvent;
            }
        }

        simulation {
            struct Clock {
                now_ns: u64,
                running: bool,
            }

            #[derive(Copy, Eq)]
            enum Control {
                Pause,
                Resume,
                Reset,
            }

            struct RobotPose {
                x_m: f64,
                y_m: f64,
                yaw_rad: f64,
            }

            struct Contact {
                in_contact: bool,
                detail: Option<String>,
            }

            topic clock: pubsub Clock;
            topic control: pubsub Control;
            topic robot_pose: pubsub RobotPose;
            topic contact: pubsub Contact;
        }

        // Per-instance component capabilities (D17/D38: framework-runtime / driver
        // territory). `component(instance)` selects a manifest-declared component;
        // each child `kind(capability)` is a self-contained node whose key is
        // `component/{instance}/<kind>/{capability}/<leaf>`. Nodes duplicate any
        // types they share by design — the node path disambiguates, so the names
        // are path-local.
        component(instance) {
            motor(capability) {
                /// A per-actuator command.
                enum Command {
                    Velocity(f32),
                    Torque(f32),
                    Stop,
                }

                topic command: pubsub Command;
            }

            encoder(capability) {
                /// Per-encoder sample on a dynamic per-instance key.
                struct Sample {
                    position_rad: f64,
                    velocity_radps: f32,
                }

                topic sample: pubsub Sample;
            }

            accelerometer(capability) {
                /// Raw accelerometer sample in the sensor-local frame in m/s^2.
                struct Sample {
                    linear_acceleration: [f32; 3],
                }

                topic sample: pubsub Sample;
            }

            gyroscope(capability) {
                /// Raw angular velocity sample in the sensor-local frame in rad/s.
                struct Sample {
                    angular_velocity: [f32; 3],
                }

                topic sample: pubsub Sample;
            }

            magnetometer(capability) {
                struct Sample {
                    magnetic_field: [f32; 3],
                }

                topic sample: pubsub Sample;
            }

            imu(capability) {
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

                struct Sample {
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

                topic sample: pubsub Sample;
            }

            range(capability) {
                #[derive(Copy, Eq)]
                #[serde(rename_all = "snake_case")]
                enum SensorHealth {
                    Nominal,
                    Degraded,
                    Fault,
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

                struct Sample {
                    distance_m: f32,
                    limits: Option<Limits>,
                    measured_at_ns: Option<u64>,
                    quality: Option<SampleQuality>,
                    health: SensorHealth,
                }

                topic sample: pubsub Sample;
            }

            gnss(capability) {
                struct Sample {
                    latitude: f64,
                    longitude: f64,
                    altitude: f64,
                    position_covariance: [f64; 9],
                }

                topic sample: pubsub Sample;
            }

            camera(capability) {
                #[derive(Copy, Eq)]
                #[serde(rename_all = "snake_case")]
                enum Encoding {
                    Jpeg,
                    Png,
                    L8,
                    Rgb8,
                    Rgba8,
                }

                #[derive(Copy)]
                struct Intrinsics {
                    fx: f32,
                    fy: f32,
                    cx: f32,
                    cy: f32,
                }

                struct Distortion {
                    model: String,
                    coefficients: Vec<f32>,
                }

                #[derive(Copy)]
                struct ExposureTiming {
                    exposure_start_ns: Option<u64>,
                    exposure_duration_ns: Option<u64>,
                }

                struct CalibrationIdentity {
                    id: String,
                    version: String,
                }

                struct Frame {
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

                topic frame: pubsub Frame;
            }

            depth(capability) {
                #[derive(Copy, Eq)]
                #[serde(rename_all = "snake_case")]
                enum Encoding {
                    U16Millimeters,
                }

                #[derive(Copy, Eq)]
                #[serde(rename_all = "snake_case")]
                enum InvalidSamplePolicy {
                    ZeroIsInvalid,
                    NonFiniteIsInvalid,
                }

                #[derive(Copy)]
                struct Intrinsics {
                    fx: f32,
                    fy: f32,
                    cx: f32,
                    cy: f32,
                }

                struct Distortion {
                    model: String,
                    coefficients: Vec<f32>,
                }

                #[derive(Copy)]
                struct ExposureTiming {
                    exposure_start_ns: Option<u64>,
                    exposure_duration_ns: Option<u64>,
                }

                struct CalibrationIdentity {
                    id: String,
                    version: String,
                }

                struct Frame {
                    samples_mm: Vec<u16>,
                    encoding: Encoding,
                    invalid_sample_policy: InvalidSamplePolicy,
                    width: Option<u32>,
                    height: Option<u32>,
                    intrinsics: Option<Intrinsics>,
                    distortion: Option<Distortion>,
                    exposure: Option<ExposureTiming>,
                    measured_at_ns: Option<u64>,
                    calibration: Option<CalibrationIdentity>,
                }

                topic frame: pubsub Frame;
            }

            lidar(capability) {
                #[derive(Copy, Eq)]
                #[serde(rename_all = "snake_case")]
                enum SensorHealth {
                    Nominal,
                    Degraded,
                    Fault,
                }

                #[derive(Copy)]
                struct ScanGeometry {
                    angle_min_rad: f32,
                    angle_increment_rad: f32,
                }

                #[derive(Copy)]
                struct RangeLimits {
                    min_m: f32,
                    max_m: f32,
                }

                #[derive(Copy)]
                struct ScanQuality {
                    valid_points: u32,
                }

                struct Ranges {
                    ranges: Vec<f32>,
                    geometry: Option<ScanGeometry>,
                    limits: Option<RangeLimits>,
                    measured_at_ns: Option<u64>,
                    quality: Option<ScanQuality>,
                    health: SensorHealth,
                }

                struct Points {
                    points: Vec<[f32; 3]>,
                    limits: Option<RangeLimits>,
                    measured_at_ns: Option<u64>,
                    quality: Option<ScanQuality>,
                    health: SensorHealth,
                }

                #[serde(tag = "kind", rename_all = "snake_case")]
                enum Scan {
                    Ranges(Ranges),
                    Points(Points),
                }

                topic scan: pubsub Scan;
            }

            mmwave(capability) {
                #[derive(Copy)]
                struct Detection {
                    position: [f32; 3],
                    velocity: [f32; 3],
                    snr: f32,
                }

                struct Scan {
                    detections: Vec<Detection>,
                }

                topic scan: pubsub Scan;
            }

            microphone(capability) {
                struct Frame {
                    data: Vec<u8>,
                }

                topic frame: pubsub Frame;
            }

            led(capability) {
                #[derive(Copy, Eq)]
                enum Command {
                    On,
                    Off,
                }

                topic command: pubsub Command;
            }

            emergency_stop(capability) {
                #[derive(Eq)]
                struct State {
                    engaged: bool,
                }

                topic state: pubsub State;
            }
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
