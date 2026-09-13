//! Native MuJoCo simulation coordinator shared by headless and desktop entry points.

#[cfg(feature = "native")]
pub mod core;

/// Public-session simulation authority and native scene coordinator.
pub mod remote;

#[cfg(all(feature = "native", feature = "desktop"))]
pub mod desktop;
