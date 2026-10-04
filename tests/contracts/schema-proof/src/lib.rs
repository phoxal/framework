//! Rust-authored message proof fixture.
//!
//! The messages below are declared once in Rust; `api/proof/v1/reference.proto`
//! is an independently compiled Protobuf reference with the same wire
//! contract. The tests prove the two agree on bytes, decoded values, and
//! standard descriptors, and that the complete schema closure is extractable
//! from this binary without executing it.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Phoxal supports Linux and macOS only");

use phoxal::contracts::component::battery::BatterySample;
use phoxal::contracts::component::encoder::EncoderSample;
use phoxal::contracts::component::lidar::LaserScan;
use phoxal::contracts::geometry::{Pose, Twist};

#[phoxal::message(package = "proof.v1")]
pub enum Mode {
    Unspecified = 0,
    Manual = 2,
}

#[phoxal::message(package = "proof.v1")]
pub enum Reason {
    Unspecified = 0,
    Unavailable = 1,
    Degraded = 3,
}

/// One actuator control command: a payload enum whose variants carry
/// scalar payloads.
#[phoxal::message(package = "proof.v1")]
pub enum Control {
    #[phoxal(tag = 1)]
    VelocityRadps(f64),
    #[phoxal(tag = 2)]
    TorqueNm(f64),
}

#[phoxal::message(package = "proof.v1")]
pub struct Target {
    #[phoxal(tag = 1)]
    pub actuator_id: String,
    #[phoxal(tag = 2)]
    pub control: Option<Control>,
}

#[phoxal::message(package = "proof.v1")]
pub struct Command {
    #[phoxal(tag = 1)]
    pub mode: Mode,
    #[phoxal(tag = 2)]
    pub owner: Option<String>,
    #[phoxal(tag = 3)]
    pub targets: Vec<Target>,
    #[phoxal(tag = 4)]
    pub payload: Vec<u8>,
    #[phoxal(tag = 5)]
    pub count: u32,
    #[phoxal(tag = 6)]
    pub depth_mm: Vec<u32>,
    #[phoxal(tag = 7)]
    pub encoder: Option<EncoderSample>,
    #[phoxal(tag = 8)]
    pub reasons: Vec<Reason>,
}

/// A private input with no authored package: the owner-qualified identity
/// is derived from this crate and module.
#[phoxal::message]
pub struct MapState {
    #[phoxal(tag = 1)]
    pub revision: u64,
    #[phoxal(tag = 2)]
    pub available: bool,
    #[phoxal(tag = 3)]
    pub oldest_capture_time_nanos: Option<u64>,
}

/// A packed-repeated vocabulary for boundary proofs.
#[phoxal::message(package = "proof.v1")]
pub struct Numbers {
    #[phoxal(tag = 1)]
    pub values: Vec<u32>,
}

/// One exported composite carrying the SDK's nested spatial geometry and
/// the additional electrical and scanning vocabularies inside an authored
/// contract: the standard types keep their canonical identities instead of
/// being copied into this package.
#[phoxal::message(package = "proof.v1")]
pub struct Telemetry {
    #[phoxal(tag = 1)]
    pub pose: Option<Pose>,
    #[phoxal(tag = 2)]
    pub twist: Option<Twist>,
    #[phoxal(tag = 3)]
    pub battery: Option<BatterySample>,
    #[phoxal(tag = 4)]
    pub scan: Option<LaserScan>,
}

/// Fixed-width packed elements for the overrun proof.
#[phoxal::message(package = "proof.v1")]
pub struct Gains {
    #[phoxal(tag = 1)]
    pub values: Vec<f32>,
}

/// A two-field payload message for the payload-enum merge proof.
#[phoxal::message(package = "proof.v1")]
pub struct Child {
    #[phoxal(tag = 1)]
    pub a: u32,
    #[phoxal(tag = 2)]
    pub b: u32,
}

/// One message- and one string-valued variant: both merge when repeated.
#[phoxal::message(package = "proof.v1")]
pub enum Parent {
    #[phoxal(tag = 1)]
    Child(Child),
    #[phoxal(tag = 2)]
    Text(String),
}

/// One package declaration for the whole exported module.
#[phoxal::messages(package = "proof.module.v1")]
pub mod v1 {
    pub enum State {
        Unspecified = 0,
        Ready = 1,
    }

    /// A payload enum nested as an ordinary message field of `Snapshot`.
    pub enum SnapshotValue {
        #[phoxal(tag = 1)]
        Reading(f64),
        #[phoxal(tag = 2)]
        Pending(f64),
    }

    pub struct Snapshot {
        #[phoxal(tag = 1)]
        pub state: State,
        #[phoxal(tag = 2)]
        pub prior: Option<State>,
        #[phoxal(tag = 3)]
        pub history: Vec<State>,
        #[phoxal(tag = 4)]
        pub value: Option<SnapshotValue>,
    }

    /// Every standalone option stays available inside the module: the wire
    /// name is renamed while the inherited package still applies.
    #[phoxal::message(schema_name = "WireReading")]
    pub struct Reading {
        #[phoxal(tag = 1)]
        pub value: f64,
    }

    /// An empty argument list also composes: the module injects its package
    /// without leaving a stray comma.
    #[phoxal::message()]
    pub struct Empty {
        #[phoxal(tag = 1)]
        pub flag: bool,
    }
}

/// The same type name in a different module derives a distinct private
/// identity.
pub mod nested {
    #[phoxal::message]
    pub struct MapState {
        #[phoxal(tag = 1)]
        pub revision: u64,
    }
}
