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

        motor {
            /// A per-actuator command.
            enum Command {
                Velocity(f32),
                Torque(f32),
                Stop,
            }

            topic command: pubsub Command;
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

            /// The asset bytes, or a not-found marker.
            enum GetResponse {
                Found { bytes: Vec<u8> },
                Missing,
            }

            topic get: query GetRequest => GetResponse;
        }
    }
}

#[cfg(test)]
mod tests;
