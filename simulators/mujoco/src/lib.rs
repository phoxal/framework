//! Native MuJoCo simulation coordinator shared by headless and desktop entry points.

pub mod core;

#[cfg(feature = "desktop")]
pub mod desktop;
