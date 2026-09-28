//! The kinematics service's payload vocabulary and endpoint contract.
//!
//! This executable authors its `phoxal.kinematics.v1` messages here,
//! private to this binary, and publishes odometry through the SDK's
//! producer-independent robotics standard; consumers bind through its
//! compiled contract products. Wire identities, field numbers, and presence match the
//! published Protobuf definitions exactly, so independently authored copies
//! in other participants compose by name.
use phoxal::contracts::robotics::OdometryState;

use phoxal::contracts::component::encoder::EncoderSample;
use phoxal::contracts::{Latest, Queue, RequestReply};

#[phoxal::messages(package = "phoxal.kinematics.v1")]
mod v1 {
    use super::{EncoderSample, Latest, OdometryState, Queue, RequestReply};

    /// The measured state of one joint.
    pub struct JointState {
        #[phoxal(tag = 1)]
        pub joint_id: String,
        #[phoxal(tag = 2)]
        pub position_rad: f64,
        #[phoxal(tag = 3)]
        pub velocity_radps: f64,
        #[phoxal(tag = 4)]
        pub effort_nm: Option<f64>,
    }

    /// One rigid transform between named frames.
    pub struct FrameTransform {
        #[phoxal(tag = 1)]
        pub parent_frame_id: String,
        #[phoxal(tag = 2)]
        pub child_frame_id: String,
        #[phoxal(tag = 3)]
        pub x_m: f64,
        #[phoxal(tag = 4)]
        pub y_m: f64,
        #[phoxal(tag = 5)]
        pub yaw_rad: f64,
    }

    /// The complete frame graph at one revision.
    pub struct FrameTree {
        #[phoxal(tag = 1)]
        pub transforms: Vec<FrameTransform>,
        #[phoxal(tag = 2)]
        pub revision: u64,
    }

    /// Why the kinematics product is unavailable.
    pub enum UnavailableReason {
        Unspecified = 0,
        Encoder = 1,
        InvalidMeasurement = 2,
        Configuration = 3,
    }

    /// The kinematics product's availability snapshot.
    pub struct KinematicsStatus {
        #[phoxal(tag = 1)]
        pub available: bool,
        #[phoxal(tag = 2)]
        pub unavailable_reasons: Vec<UnavailableReason>,
        #[phoxal(tag = 3)]
        pub revision: u64,
    }

    /// One frame-graph lookup request.
    pub struct LookupFrameRequest {
        #[phoxal(tag = 1)]
        pub parent_frame_id: String,
        #[phoxal(tag = 2)]
        pub child_frame_id: String,
        #[phoxal(tag = 3)]
        pub revision: u64,
    }

    /// One frame-graph lookup response.
    pub struct LookupFrameResponse {
        #[phoxal(tag = 1)]
        pub transform: Option<FrameTransform>,
        #[phoxal(tag = 2)]
        pub revision: u64,
    }

    /// The kinematics service's endpoint contract.
    #[phoxal::endpoints]
    pub struct KinematicsApi {
        #[phoxal::input(max_items = 32, max_bytes = 262_144)]
        encoders: Queue<EncoderSample>,

        #[phoxal::output(max_items = 32, max_bytes = 16_384)]
        joints: Queue<JointState>,

        #[phoxal::output(projection = state, bootstrap, on_change, max_bytes = 512)]
        odometry: Latest<OdometryState>,

        #[phoxal::output(projection = state, bootstrap, on_change, max_bytes = 4096)]
        frames: Latest<FrameTree>,

        #[phoxal::output(projection = state, bootstrap, on_change, max_bytes = 512)]
        status: Latest<KinematicsStatus>,

        #[phoxal::operation(max_items = 32, max_bytes = 16_384)]
        lookup_frame: RequestReply<LookupFrameRequest, LookupFrameResponse>,
    }
}

pub use v1::*;
