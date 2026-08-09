//! v0.2 safety payloads.
#![allow(legacy_derive_helpers)]

            pub use crate::domains::v0_1::safety::ConstraintSource;
            /// Why safety is stopping or limiting body motion in v0.2. The
            /// map/footprint reasons are deliberately distinct so a fail
            /// closed decision remains diagnosable without parsing text.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(rename_all = "snake_case")]
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
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(rename_all = "snake_case")]
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
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(try_from = "crate::SafetyConstraintWire")]
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
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(try_from = "crate::SafetyMotionPermissionWire")]
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

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(try_from = "crate::SafetyMotionConstraintsWire")]
            pub struct MotionConstraints {
                pub sequence: u64,
                pub permission: MotionPermission,
                pub constraints: Vec<Constraint>,
                pub expires_at: ::phoxal_bus::RobotInstant,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            pub struct State {
                pub constraints: MotionConstraints,
            }
