//! The world service's payload vocabulary and endpoint contract.
//!
//! Every `phoxal.world.v1` message this executable speaks is authored here,
//! private to this binary; the consumed odometry input is the SDK's
//! producer-independent robotics standard. Consumers bind through this
//! binary's compiled contract products.
use phoxal::contracts::robotics::OdometryState;

use phoxal::contracts::{Latest, RequestReply};

#[phoxal::messages(package = "phoxal.world.v1")]
mod v1 {
    use super::{Latest, OdometryState, RequestReply};

    /// One wheel or joint encoder integrated into a body pose.
    ///
    /// The tracked world pose belief.
    pub struct WorldBelief {
        #[phoxal(tag = 1)]
        pub frame_id: String,
        #[phoxal(tag = 2)]
        pub x_m: f64,
        #[phoxal(tag = 3)]
        pub y_m: f64,
        #[phoxal(tag = 4)]
        pub yaw_rad: f64,
        #[phoxal(tag = 5)]
        pub confidence: f32,
        #[phoxal(tag = 6)]
        pub revision: u64,
        #[phoxal(tag = 7)]
        pub available: bool,
        /// Oldest source capture supporting this product, in the execution timeline.
        /// Republishing or deriving state must preserve this time.
        #[phoxal(tag = 8)]
        pub oldest_capture_time_nanos: Option<u64>,
    }

    /// The world product's revision snapshot.
    pub struct WorldRevision {
        #[phoxal(tag = 1)]
        pub revision: u64,
        #[phoxal(tag = 2)]
        pub available: bool,
        /// Oldest source capture supporting this product, in the execution timeline.
        /// Republishing or deriving state must preserve this time.
        #[phoxal(tag = 3)]
        pub oldest_capture_time_nanos: Option<u64>,
    }

    /// An axis-aligned rectangular bound.
    pub struct Bounds {
        #[phoxal(tag = 1)]
        pub min_x_m: f64,
        #[phoxal(tag = 2)]
        pub min_y_m: f64,
        #[phoxal(tag = 3)]
        pub max_x_m: f64,
        #[phoxal(tag = 4)]
        pub max_y_m: f64,
    }

    /// One grid cell's occupancy.
    pub enum Occupancy {
        Unspecified = 0,
        Free = 1,
        Occupied = 2,
        Unknown = 3,
    }

    /// One rectangular window of the occupancy grid.
    pub struct GridWindow {
        #[phoxal(tag = 1)]
        pub frame_id: String,
        #[phoxal(tag = 2)]
        pub origin_x_m: f64,
        #[phoxal(tag = 3)]
        pub origin_y_m: f64,
        #[phoxal(tag = 4)]
        pub resolution_m: f64,
        #[phoxal(tag = 5)]
        pub width: u32,
        #[phoxal(tag = 6)]
        pub height: u32,
        #[phoxal(tag = 7)]
        pub cells: Vec<Occupancy>,
        #[phoxal(tag = 8)]
        pub revision: u64,
        #[phoxal(tag = 9)]
        pub requested: Option<Bounds>,
        #[phoxal(tag = 10)]
        pub covered: Option<Bounds>,
    }

    /// Why a window request cannot be served.
    pub enum WindowUnavailableReason {
        Unspecified = 0,
        WorldUnavailable = 1,
        OutOfBounds = 2,
        RevisionNotRetained = 3,
    }

    /// The unavailable variant of a window response.
    pub struct WindowUnavailable {
        #[phoxal(tag = 1)]
        pub reason: WindowUnavailableReason,
        #[phoxal(tag = 2)]
        pub revision: u64,
    }

    /// One occupancy window request.
    pub struct WindowRequest {
        #[phoxal(tag = 1)]
        pub requested: Option<Bounds>,
        #[phoxal(tag = 2)]
        pub revision: u64,
    }

    /// One occupancy window response.
    pub enum WindowResponse {
        #[phoxal(tag = 1)]
        Window(GridWindow),
        #[phoxal(tag = 2)]
        Unavailable(WindowUnavailable),
    }

    /// Why the world product is unavailable.
    pub enum UnavailableReason {
        Unspecified = 0,
        Pose = 1,
        StalePose = 2,
        InvalidPose = 3,
    }

    /// The world product's availability snapshot.
    pub struct WorldStatus {
        #[phoxal(tag = 1)]
        pub available: bool,
        #[phoxal(tag = 2)]
        pub unavailable_reasons: Vec<UnavailableReason>,
        #[phoxal(tag = 3)]
        pub revision: u64,
    }

    /// The world service's endpoint contract.
    #[phoxal::endpoints]
    pub struct WorldApi {
        #[phoxal::input(max_age_ms = 100, max_bytes = 512)]
        pose: Latest<OdometryState>,

        #[phoxal::output(projection = state, bootstrap, on_change, max_bytes = 512)]
        belief: Latest<WorldBelief>,

        #[phoxal::output(projection = state, bootstrap, on_change, max_bytes = 128)]
        revision: Latest<WorldRevision>,

        #[phoxal::output(projection = state, bootstrap, on_change, max_bytes = 512)]
        status: Latest<WorldStatus>,

        #[phoxal::operation(max_items = 32, max_bytes = 16_384)]
        window: RequestReply<WindowRequest, WindowResponse>,
    }
}

pub use v1::*;
