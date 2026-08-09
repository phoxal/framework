//! Payloads owned by this wire revision.
#![allow(legacy_derive_helpers)]

        pub mod drive {

            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum StopReason {
                TargetStale,
                TargetNotFinite,
                ActuatorCommandNotFinite,
                EmergencyStop,
                Fault,
            }

            /// A finite requested or limited planar velocity in the current
            /// control wire revision.
            #[serde(deny_unknown_fields)]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Target {
                #[serde(deserialize_with = "crate::deserialize_finite_target_scalar")]
                pub(crate) linear_x_mps: f32,
                #[serde(deserialize_with = "crate::deserialize_finite_target_scalar")]
                pub(crate) angular_z_radps: f32,
            }

            /// The drive participant's exclusive control state.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum State {
                Active {
                    target: Target,
                    limited_target: Target,
                },
                Stopped {
                    target: Target,
                    reason: StopReason,
                },
            }
        }

        pub mod motion {

            /// The sole motion execution decision. A stopped decision carries
            /// no source or target, so consumers cannot observe an active
            /// source alongside a stop reason.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum Decision {
                Active {
                    source: Source,
                    target: crate::v0_2::drive::Target,
                },
                Stopped {
                    reason: ZeroReason,
                },
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct State {
                pub decision: Decision,
                /// How long ago motion observed the live manual command, on
                /// its own host clock. `None` when no manual command is live.
                pub manual_observed_age_ns: Option<u64>,
                pub autonomous_candidate_age_ns: Option<u64>,
                pub safety_constraints_age_ns: Option<u64>,
                pub safety_runtime: SafetyRuntime,
                pub component_estop_blocked: bool,
                pub active_safety_constraints: Vec<super::safety::Constraint>,
                pub safety_permission: super::safety::MotionPermission,
            }
        }

        pub mod perception {
            /// A current-revision detection with wire-level finite and fixed
            /// shape guarantees. The v0.1 body above remains untouched.
            #[serde(deny_unknown_fields)]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Detection {
                pub class_id: String,
                #[serde(deserialize_with = "crate::deserialize_finite_detection_confidence")]
                pub confidence: f32,
                #[serde(deserialize_with = "crate::deserialize_finite_detection_position")]
                pub position_m: [f64; 3],
                pub frame_id: String,
                pub track_id: Option<u64>,
            }

            /// One source-captured perception batch. `captured_at` is copied
            /// from the selected camera measurement's provenance; it is not
            /// the perception step's publication instant.
            #[serde(deny_unknown_fields)]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Detections {
                pub source: crate::SourceRef,
                pub captured_at: ::phoxal_bus::TimeWindow,
                pub detections: Vec<Detection>,
            }

            /// Why the perception participant cannot provide a healthy batch.
            #[derive(Copy, Eq)]
            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum HealthReason {
                MissingCamera,
                StaleCamera,
                InvalidCamera,
                DetectorFailure,
                BackendUnavailable,
                PublicationFailure,
                ManagedInputFailure,
            }

            /// The perception participant's exclusive published health.
            #[serde(deny_unknown_fields)]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum State {
                Healthy { detector: String },
                Unhealthy {
                    detector: String,
                    reason: HealthReason,
                },
            }
        }

        pub mod navigation {

            /// A navigation server-issued operation identity.  The producer
            /// scopes the local sequence to this service incarnation, so a
            /// restart inside one execution cannot collide with an old
            /// operation that used the same counter value.
            #[derive(Copy, Eq, Hash)]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct NavigationOperationId {
                pub producer: ::phoxal_bus::ProducerId,
                pub sequence: u64,
            }

            /// Work accepted by the navigation server.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum StartKind {
                GotoPose(Pose),
                FollowPath(Path),
            }

            /// A start admission request. The requester producer comes from
            /// the trusted query envelope. An accepted `(requester,
            /// request_id)` is idempotent while the server retains it: stock
            /// navigation keeps the 1,024 most recent accepted admissions
            /// globally. Refusals are current-state responses and are not
            /// retained. A client must not retry an accepted request after it
            /// has been evicted, or across a navigation reset/restart.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct StartRequest {
                pub request_id: RequestId,
                pub kind: StartKind,
            }

            /// The server's idempotent admission response.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum StartResponse {
                Accepted { operation_id: NavigationOperationId },
                Refused(RefusalReason),
            }

            /// Cancel one server-issued operation. The query requester must
            /// be the operation owner. A terminal operation remains
            /// idempotently cancellable only while stock navigation retains
            /// it in its global 1,024-operation completion window; after
            /// eviction the server reports `NotFound`.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct CancelRequest {
                pub operation_id: NavigationOperationId,
            }

            /// The cancellation admission response.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum CancelResponse {
                Accepted,
                Refused(RefusalReason),
            }

            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum RefusalReason {
                Busy,
                InvalidRequest,
                Unsupported,
                Unavailable,
                NotOwner,
                NotFound,
            }

            // Results are ordered completion events, not a latest-value
            // snapshot. Keep the published v0.1 role immutable and correct the
            // active revision's delivery family explicitly.

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum State {
                Idle,
                Accepted(NavigationOperationId),
                Running(NavigationOperationId),
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Progress {
                pub operation_id: NavigationOperationId,
                pub request_id: RequestId,
                pub distance_remaining_m: f64,
                pub path_index: u32,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Result {
                pub operation_id: NavigationOperationId,
                pub request_id: RequestId,
                pub outcome: Outcome,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Candidate {
                pub operation_id: NavigationOperationId,
                pub linear_x_mps: f32,
                pub angular_z_radps: f32,
            }

        }

        pub mod safety {
            /// Why safety is stopping or limiting body motion in v0.2. The
            /// map/footprint reasons are deliberately distinct so a fail
            /// closed decision remains diagnosable without parsing text.
            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum ConstraintReason {
                WorldUnavailable,
                MapUnavailable,
                MapStale,
                MapPartial,
                MapRevisionInvalid,
                UnknownOccupancy,
                FootprintUnavailable,
                FootprintMismatch,
                FootprintObstacle,
                DrivableSpaceUnavailable,
                LocalizationUnavailable,
                LocalizationUncertain,
                ObstacleProximity,
                RangeSensorFault,
                DriveFault,
                BatteryLow,
                BatteryCritical,
                BatteryUnavailable,
                BatteryStale,
                SpeedZone,
                OperatorPolicy,
            }

            /// Additional provenance category for the compiled footprint
            /// checks. The source is still the safety participant, but the
            /// category tells operators which safety input failed.
            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum ConstraintSourceKind {
                WorldModel,
                Map,
                Localization,
                Range,
                Drive,
                Battery,
                Footprint,
                Operator,
            }

            /// A constraint is one shape only: a `Limited` item carries
            /// effective limits and a `Stopped` item carries no contradictory
            /// limit fields. This prevents a consumer from having to choose
            /// between `stop = true` and a nonzero limit.
            #[serde(try_from = "crate::SafetyConstraintWire")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum Constraint {
                Limited {
                    reason: ConstraintReason,
                    source: ConstraintSource,
                    max_linear_speed_mps: f32,
                    max_angular_speed_radps: f32,
                    observed_value: Option<f32>,
                    valid_from: ::phoxal_bus::RobotInstant,
                    expires_at: ::phoxal_bus::RobotInstant,
                },
                Stopped {
                    reason: ConstraintReason,
                    source: ConstraintSource,
                    observed_value: Option<f32>,
                    valid_from: ::phoxal_bus::RobotInstant,
                    expires_at: ::phoxal_bus::RobotInstant,
                },
            }

            /// The sole safety permission consumed by motion. Its variants
            /// are mutually exclusive and carry exactly the fields that make
            /// sense for that decision.
            #[serde(try_from = "crate::SafetyMotionPermissionWire")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum MotionPermission {
                Clear,
                Limited {
                    effective_linear_speed_mps: f32,
                    effective_angular_speed_radps: f32,
                    reasons: Vec<ConstraintReason>,
                },
                Stopped {
                    reasons: Vec<ConstraintReason>,
                },
            }

            #[serde(try_from = "crate::SafetyMotionConstraintsWire")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct MotionConstraints {
                pub sequence: u64,
                pub permission: MotionPermission,
                pub constraints: Vec<Constraint>,
                pub expires_at: ::phoxal_bus::RobotInstant,
            }

            #[serde(deny_unknown_fields)]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct State {
                pub constraints: MotionConstraints,
            }
        }

        pub mod map {
            /// A finite world-space point used as the cell origin and pose
            /// translation in a self-describing grid response.
            #[serde(try_from = "crate::GridPointWire")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Point {
                pub x_m: f64,
                pub y_m: f64,
            }

            /// The map-frame pose of the grid's reference origin.
            #[serde(try_from = "crate::GridPoseWire")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Pose {
                pub x_m: f64,
                pub y_m: f64,
                pub yaw_rad: f64,
            }

            /// Requested and covered map-frame bounds.
            #[serde(try_from = "crate::GridBoundsWire")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Bounds {
                pub min_x_m: f64,
                pub min_y_m: f64,
                pub max_x_m: f64,
                pub max_y_m: f64,
            }

            /// Occupancy has a closed wire domain. Unknown is not treated as
            /// free by safety or navigation.
            #[serde(rename_all = "snake_case")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum Occupancy {
                Free,
                Occupied,
                Unknown,
            }

            /// A revisioned map window whose origin, frame, extent and bounds
            /// travel with the cells themselves.
            #[serde(try_from = "crate::GridWindowWire")]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct GridWindow {
                pub frame_id: String,
                pub origin_pose: Pose,
                pub cell_origin: Point,
                #[serde(deserialize_with = "crate::deserialize_finite_positive_resolution")]
                pub resolution_m: f32,
                #[serde(deserialize_with = "crate::deserialize_nonzero_map_dimension")]
                pub width: u32,
                #[serde(deserialize_with = "crate::deserialize_nonzero_map_dimension")]
                pub height: u32,
                pub cells: Vec<Occupancy>,
                pub revision: u64,
                pub requested: Bounds,
                pub covered: Bounds,
            }

            /// A query either returns a complete window, a clipped window
            /// with explicit requested/covered bounds, or an explicit
            /// out-of-bounds result. A responder may not silently substitute
            /// a different extent for what was requested.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum SubmapResponse {
                Window(GridWindow),
                Partial { window: GridWindow },
                OutOfBounds {
                    requested: Bounds,
                    #[serde(deserialize_with = "crate::deserialize_nonempty_frame_id")]
                    frame_id: String,
                    revision: u64,
                },
            }

        }

        pub mod component {
            pub mod speaker {
            }

            pub mod motor {
                /// A per-actuator command in the current control revision.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub enum Command {
                    Position(#[serde(deserialize_with = "crate::deserialize_finite_motor_scalar")] f32),
                    Velocity(#[serde(deserialize_with = "crate::deserialize_finite_motor_scalar")] f32),
                    Torque(#[serde(deserialize_with = "crate::deserialize_finite_motor_scalar")] f32),
                    Stop,
                }
            }
        }
    }


