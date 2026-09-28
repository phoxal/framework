//! Standard component contracts, one module per device capability kind.
//!
//! A component's declared capabilities determine its standard endpoints;
//! these payloads are the vocabulary those endpoints share across concrete
//! drivers and the native simulator. Device-specific behavior stays in the
//! owning component's private modules.

/// The actuator command vocabulary of `motor` capabilities.
pub mod actuator;

/// The encoder observation vocabulary of `encoder` capabilities.
pub mod encoder;

/// The range observation vocabulary of `range` capabilities.
pub mod range;

/// The inertial measurement vocabulary of `imu`, `accelerometer`, and
/// `gyroscope` capabilities.
pub mod imu;

/// The visual vocabulary of `camera` capabilities (mono/RGB frames and
/// depth frames).
pub mod camera;

/// The satellite-fix vocabulary of `gnss` capabilities.
pub mod gnss;

/// Electrical observations. No automatic capability endpoint is generated yet.
pub mod battery;

/// Planar laser scans. No automatic capability endpoint is generated yet.
pub mod lidar;
