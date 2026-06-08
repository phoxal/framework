#![allow(clippy::module_name_repetitions)]

use crate::api::component::v1::capability::profile::{CameraProfileEncoding, DepthProfileEncoding};

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeStreamDemand {
    Camera(CameraStreamDemand),
    Depth(DepthStreamDemand),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CameraStreamDemand {
    pub role: &'static str,
    pub min_rate_hz: f64,
    pub min_width_px: u32,
    pub min_height_px: u32,
    pub accepted_encodings: Vec<CameraProfileEncoding>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DepthStreamDemand {
    pub role: &'static str,
    pub min_rate_hz: f64,
    pub min_width_px: u32,
    pub min_height_px: u32,
    pub accepted_encodings: Vec<DepthProfileEncoding>,
}
