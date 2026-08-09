//! Payloads owned by this wire revision.
#![allow(legacy_derive_helpers)]

        pub mod drive {
            /// Why actuation authority is in its current state.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum StopReason {
                /// Nothing is live: no target has been accepted, the producer
                /// has gone silent past the host deadline, or the held command
                /// exceeded its logical hold horizon. All three are the same
                /// fact to a consumer - the drive is not being commanded.
                TargetStale,
                TargetNotFinite,
                ActuatorCommandNotFinite,
                Inactive,
                EmergencyStop,
                Fault,
            }

            /// Whether the drive is actively commanding the actuators.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum ActuatorAuthority {
                Active,
                Stopped,
            }

            /// A requested or limited planar velocity.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Target {
                pub linear_x_mps: f32,
                pub angular_z_radps: f32,
                pub curvature_limit_radpm: Option<f32>,
            }

            /// The drive participant's published control state.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct State {
                pub target: Target,
                pub limited_target: Target,
                pub actuator_authority: ActuatorAuthority,
                pub stop_reason: Option<StopReason>,
            }

        }

        pub mod joint {
            /// Per-joint position/velocity (and optional effort) on a dynamic
            /// per-joint key.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct JointState {
                pub position_rad: f64,
                pub velocity_radps: f64,
                pub effort_nm: Option<f64>,
            }

        }

        pub mod frame {
            /// A parent → child rigid transform (translation + xyzw quaternion).
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct FrameTransform {
                pub parent_frame_id: String,
                pub child_frame_id: String,
                pub translation_m: [f64; 3],
                pub rotation_quat_xyzw: [f64; 4],
                /// When this transform was observed. Absent for a static
                /// transform, which is configuration rather than observation.
                pub stamp: Option<::phoxal_bus::RobotInstant>,
            }

            /// Transforms that do not change over time.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct StaticTransforms {
                pub transforms: Vec<FrameTransform>,
            }

            /// The current transform tree.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Tree {
                pub transforms: Vec<FrameTransform>,
            }

            /// Ask for the transform between two frames, optionally at a time.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct LookupRequest {
                pub target_frame_id: String,
                pub source_frame_id: String,
                /// The instant to resolve at. The frame service returns the
                /// latest dynamic sample at or before this instant and never a
                /// future sample. Absent asks for the greatest retained time.
                pub at: Option<::phoxal_bus::RobotInstant>,
            }

            /// The resolved transform, or `None` if it is not available.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct LookupResponse {
                pub transform: Option<FrameTransform>,
            }

        }

        pub mod power {
            /// A platform power command.
            #[derive(Copy, Eq)]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum Command {
                Reboot,
                Shutdown,
            }

            /// Where the power participant is in handling a command.
            #[derive(Copy, Eq)]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum Status {
                Idle,
                Rebooting,
                ShuttingDown,
                Failed,
            }

            /// Why a power command was rejected outright.
            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum RejectedReason {
                HostIntegrationUnavailable,
                CommandRejected,
            }

            /// Why an accepted power command later failed.
            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum FailedReason {
                HostCommandFailed,
            }

            /// The power participant's published state.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct State {
                pub status: Status,
                pub detail: Option<String>,
            }

        }

        pub mod motion {
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Target {
                pub linear_x_mps: f32,
                pub angular_z_radps: f32,
                pub curvature_limit_radpm: Option<f32>,
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum Source {
                Manual,
                Navigation,
                EmergencyStop,
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum ZeroReason {
                NoCandidate,
                NavigationCandidateStale,
                ManualCandidateNotFinite,
                NavigationCandidateNotFinite,
                EmergencyStopEngaged,
                SafetyConstraintsUnavailable,
                SafetyProtectiveStop,
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum SafetyRuntime {
                Absent,
                Present,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct ManualCommand {
                pub linear_x_mps: f64,
                pub angular_z_radps: f64,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct State {
                /// How long ago motion observed the live manual command, on
                /// its own host clock. `None` when no manual command is live.
                pub manual_observed_age_ns: Option<u64>,
                pub autonomous_candidate_age_ns: Option<u64>,
                pub safety_constraints_age_ns: Option<u64>,
                pub selected_source: Option<Source>,
                pub final_target: Target,
                pub zero_reason: Option<ZeroReason>,
                pub safety_runtime: SafetyRuntime,
                pub component_estop_blocked: bool,
                pub active_safety_constraints: Vec<super::safety::Constraint>,
            }

        }

        pub mod safety {
            /// Why safety is stopping or limiting body motion.
            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum ConstraintReason {
                WorldUnavailable,
                MapUnavailable,
                DrivableSpaceUnavailable,
                LocalizationUnavailable,
                LocalizationUncertain,
                ObstacleProximity,
                RangeSensorFault,
                DriveFault,
                BatteryLow,
                BatteryCritical,
                SpeedZone,
                OperatorPolicy,
            }

            /// Typed origin of one constraint, suitable for operator diagnosis.
            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum ConstraintSourceKind {
                WorldModel,
                Map,
                Localization,
                Range,
                Drive,
                Battery,
                Operator,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct ConstraintSource {
                pub kind: ConstraintSourceKind,
                pub participant_id: String,
                pub component_id: Option<String>,
                pub capability_id: Option<String>,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Constraint {
                pub reason: ConstraintReason,
                pub source: ConstraintSource,
                pub stop: bool,
                pub max_linear_speed_mps: Option<f32>,
                pub max_angular_speed_radps: Option<f32>,
                pub observed_value: Option<f32>,
                /// The instant this constraint starts applying, on the
                /// publisher's timeline. A consumer on another timeline gets a
                /// checked error, never a silently wrong comparison.
                pub valid_from: ::phoxal_bus::RobotInstant,
                /// The instant this constraint stops applying.
                pub expires_at: ::phoxal_bus::RobotInstant,
            }

            /// The sole safety-to-motion control product. Motion accepts it only
            /// on the same timeline and before `expires_at`.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct MotionConstraints {
                pub sequence: u64,
                pub stop: bool,
                pub max_linear_speed_mps: Option<f32>,
                pub max_angular_speed_radps: Option<f32>,
                pub constraints: Vec<Constraint>,
                pub expires_at: ::phoxal_bus::RobotInstant,
            }

            /// Operator-facing state mirrors the exact product consumed by motion.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct State {
                pub clear: bool,
                pub motion: MotionConstraints,
            }

        }

        pub mod navigation {
            /// A caller-chosen identifier for one navigation request.
            ///
            /// `Ord` is derived so a consumer can key an ordered map on the
            /// identity itself rather than on a copy of its inner `String`,
            /// which is what keeps the newtype meaningful past the point where
            /// requests are tracked. The derives add no bytes to the wire.
            #[derive(Eq, PartialOrd, Ord)]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct RequestId {
                pub value: String,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Pose {
                pub x_m: f64,
                pub y_m: f64,
                pub yaw_rad: Option<f64>,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Path {
                pub poses: Vec<Pose>,
                pub map_revision: Option<u64>,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum RequestKind {
                GotoPose(Pose),
                FollowPath(Path),
                Cancel(RequestId),
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Request {
                pub request_id: RequestId,
                pub kind: RequestKind,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum State {
                Idle,
                Accepted(RequestId),
                Running(RequestId),
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum FailureReason {
                LocalizationUnavailable,
                MapUnavailable,
                MapChanged,
                NoPath,
                Blocked,
                Internal,
            }

            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum RefusalReason {
                Busy,
                InvalidRequest,
                Unsupported,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum Outcome {
                Succeeded,
                Failed(FailureReason),
                Refused(RefusalReason),
                Cancelled,
                TimedOut,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Progress {
                pub request_id: RequestId,
                pub distance_remaining_m: f64,
                pub path_index: u32,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Result {
                pub request_id: RequestId,
                pub outcome: Outcome,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Candidate {
                pub request_id: RequestId,
                pub linear_x_mps: f32,
                pub angular_z_radps: f32,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct FrontierRequest {
                pub map_revision: Option<u64>,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Frontier {
                pub x_m: f64,
                pub y_m: f64,
                pub score: f32,
                pub size: u32,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct FrontierResponse {
                pub frontier: Option<Frontier>,
                pub map_revision: Option<u64>,
            }

        }

        pub mod perception {
            /// A single detected object: class, confidence, and pose in a frame.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Detection {
                pub class_id: String,
                pub confidence: f32,
                pub position_m: [f64; 3],
                pub frame_id: String,
                pub track_id: Option<u64>,
            }

            /// A batch of detections from one perception cycle.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Detections {
                pub detections: Vec<Detection>,
                /// The frame instant these detections were derived from.
                pub stamp: Option<::phoxal_bus::RobotInstant>,
            }

            /// The perception participant's published health.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct State {
                pub healthy: bool,
                pub detector: String,
            }

        }

        pub mod video {
            /// Ask to open a video stream for one exact camera capability at an
            /// optional size. The pre-v1 backend currently has no encoded
            /// transport, so the response reports that outcome instead of
            /// fabricating a stream identity or lifecycle.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct OpenRequest {
                pub source: crate::VideoSourceRef,
                pub width_px: Option<u32>,
                pub height_px: Option<u32>,
            }

            /// Why a requested video stream could not be opened.
            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum OpenOutcome {
                /// The source exists, but no encoder/transport backend exists
                /// in this train.
                Unsupported,
                /// No camera source is currently available.
                Unavailable,
            }

        }

        // Per-instance component capabilities: framework participant / driver
        // territory. `component(instance)` selects a manifest-declared component;
        // each child `kind(capability)` is a self-contained node whose key is
        // `component/{instance}/<kind>/{capability}/<leaf>`. Nodes duplicate any
        // types they share by design - the node path disambiguates, so the names
        // are path-local.
        pub mod component {
            pub mod motor {
                /// A per-actuator command.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub enum Command {
                    Velocity(f32),
                    Torque(f32),
                    Stop,
                }

            }

            pub mod encoder {
                /// Per-encoder sample on a dynamic per-instance key.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Sample {
                    pub position_rad: f64,
                    pub velocity_radps: f32,
                }

            }

            pub mod accelerometer {
                /// Raw accelerometer sample in the sensor-local frame in m/s^2.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Sample {
                    pub linear_acceleration: [f32; 3],
                }

            }

            pub mod gyroscope {
                /// Raw angular velocity sample in the sensor-local frame in rad/s.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Sample {
                    pub angular_velocity: [f32; 3],
                }

            }

            pub mod magnetometer {
                /// Raw magnetic-field sample in the sensor-local frame.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Sample {
                    pub magnetic_field: [f32; 3],
                }

            }

            pub mod imu {
                #[derive(Copy, Eq)]
                #[serde(rename_all = "snake_case")]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub enum SensorHealth {
                    Nominal,
                    Degraded,
                    Fault,
                }

                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Bias {
                    pub angular_velocity_radps: [f32; 3],
                    pub linear_acceleration_mps2: [f32; 3],
                }

                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Sample {
                    pub orientation: Option<[f32; 4]>,
                    pub angular_velocity_radps: [f32; 3],
                    pub linear_acceleration_mps2: [f32; 3],
                    pub covariance: Option<[f32; 9]>,
                    pub noise_density: Option<[f32; 3]>,
                    pub sensor_frame_id: Option<String>,
                    pub health: SensorHealth,
                    pub bias: Option<Bias>,
                }

            }

            pub mod range {
                #[derive(Copy, Eq)]
                #[serde(rename_all = "snake_case")]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub enum SensorHealth {
                    Nominal,
                    Degraded,
                    Fault,
                }

                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Limits {
                    pub min_m: f32,
                    pub max_m: f32,
                }

                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct SampleQuality {
                    pub valid: bool,
                    pub confidence: Option<f32>,
                }

                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Sample {
                    pub distance_m: f32,
                    pub limits: Option<Limits>,
                    pub quality: Option<SampleQuality>,
                    pub health: SensorHealth,
                }

            }

            pub mod gnss {
                /// A GNSS fix: geodetic position plus a 3x3 position covariance.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Sample {
                    pub latitude: f64,
                    pub longitude: f64,
                    pub altitude: f64,
                    pub position_covariance: [f64; 9],
                }

            }

            pub mod camera {
                #[derive(Copy, Eq)]
                #[serde(rename_all = "snake_case")]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub enum Encoding {
                    Jpeg,
                    Png,
                    L8,
                    Rgb8,
                    Rgba8,
                }

                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct ExposureTiming {
                    pub exposure_start_ns: Option<u64>,
                    pub exposure_duration_ns: Option<u64>,
                }

                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct CalibrationIdentity {
                    pub id: String,
                    pub version: String,
                }

                /// One camera frame: encoded pixel bytes plus optional calibration
                /// and timing metadata.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

            }

            pub mod depth {
                #[derive(Copy, Eq)]
                #[serde(rename_all = "snake_case")]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub enum Encoding {
                    U16Millimeters,
                }

                #[derive(Copy, Eq)]
                #[serde(rename_all = "snake_case")]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub enum InvalidSamplePolicy {
                    ZeroIsInvalid,
                    NonFiniteIsInvalid,
                }

                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct ExposureTiming {
                    pub exposure_start_ns: Option<u64>,
                    pub exposure_duration_ns: Option<u64>,
                }

                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct CalibrationIdentity {
                    pub id: String,
                    pub version: String,
                }

                /// One depth frame: per-pixel millimetre samples plus optional
                /// calibration and timing metadata.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Frame {
                    pub samples_mm: Vec<u16>,
                    pub encoding: Encoding,
                    pub invalid_sample_policy: InvalidSamplePolicy,
                    pub width: Option<u32>,
                    pub height: Option<u32>,
                    pub intrinsics: Option<Intrinsics>,
                    pub distortion: Option<Distortion>,
                    pub exposure: Option<ExposureTiming>,
                    pub calibration: Option<CalibrationIdentity>,
                }

            }

            pub mod lidar {
                #[derive(Copy, Eq)]
                #[serde(rename_all = "snake_case")]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub enum SensorHealth {
                    Nominal,
                    Degraded,
                    Fault,
                }

                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct ScanGeometry {
                    pub angle_min_rad: f32,
                    pub angle_increment_rad: f32,
                }

                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct RangeLimits {
                    pub min_m: f32,
                    pub max_m: f32,
                }

                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct ScanQuality {
                    pub valid_points: u32,
                }

                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Ranges {
                    pub ranges: Vec<f32>,
                    pub geometry: Option<ScanGeometry>,
                    pub limits: Option<RangeLimits>,
                    pub quality: Option<ScanQuality>,
                    pub health: SensorHealth,
                }

                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Points {
                    pub points: Vec<[f32; 3]>,
                    pub limits: Option<RangeLimits>,
                    pub quality: Option<ScanQuality>,
                    pub health: SensorHealth,
                }

                /// One lidar scan, either as polar ranges or as cartesian points.
                #[serde(tag = "kind", rename_all = "snake_case")]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub enum Scan {
                    Ranges(Ranges),
                    Points(Points),
                }

            }

            pub mod mmwave {
                /// One mmWave radar detection: position, velocity, and SNR.
                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Detection {
                    pub position: [f32; 3],
                    pub velocity: [f32; 3],
                    pub snr: f32,
                }

                /// One mmWave radar scan as a set of detections.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Scan {
                    pub detections: Vec<Detection>,
                }

            }

            pub mod microphone {
                /// One audio frame as raw encoded bytes.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Frame {
                    pub data: Vec<u8>,
                }

            }

            pub mod led {
                /// A per-LED on/off command.
                #[derive(Copy, Eq)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub enum Command {
                    On,
                    Off,
                }

            }

            pub mod speaker {
                /// One chunk of an audio stream to play on this speaker.
                ///
                /// `Some(bytes)` carries WAV-coded audio: the first chunk of a
                /// stream starts with the standard WAV header, later chunks
                /// continue its data. `None` ends the stream and is what tells
                /// the owner the sound is complete.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Chunk {
                    pub stream: Option<Vec<u8>>,
                }

            }

            pub mod battery {
                /// Battery state reported by the pack's owner - the simulator
                /// backing this capability, or the real driver.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct State {
                    pub voltage_v: f32,
                    pub current_a: f32,
                    pub charge_ratio: f32,
                }

            }

            pub mod emergency_stop {
                /// Per-instance emergency-stop state.
                #[derive(Eq)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct State {
                    pub engaged: bool,
                }

            }
        }

        pub mod odometry {
            /// A planar pose + twist estimate in the odometry frame.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct State {
                pub x_m: f64,
                pub y_m: f64,
                pub yaw_rad: f64,
                pub linear_x_mps: f32,
                pub angular_z_radps: f32,
            }

        }

        pub mod localize {
            /// A planar localization estimate in the map frame.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct LocalizationState {
                pub x_m: f64,
                pub y_m: f64,
                pub yaw_rad: f64,
                pub confidence: f32,
            }

        }

        pub mod map {
            /// A published map revision marker.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Revision {
                pub revision: u64,
                pub resolution_m: f32,
            }

            /// Request a rectangular submap window (map-frame metres).
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct SubmapRequest {
                pub min_x_m: f64,
                pub min_y_m: f64,
                pub max_x_m: f64,
                pub max_y_m: f64,
            }

            /// An occupancy-grid window: row-major cells, 0..=100 + 255 = unknown.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct SubmapResponse {
                pub width: u32,
                pub height: u32,
                pub resolution_m: f32,
                pub cells: Vec<u8>,
            }

        }



