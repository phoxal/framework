#![allow(clippy::module_name_repetitions)]

use crate::api::component::v1::{
    CameraStreamDemand, DepthStreamDemand, RuntimeStreamDemand,
    capability::profile::{CameraProfileEncoding, DepthProfileEncoding},
};
use crate::model::robot::v1::{LocalizeBackendKind, Role};

pub struct LocalizeStreamDemands;

impl LocalizeStreamDemands {
    pub fn for_backend(backend: LocalizeBackendKind) -> Vec<RuntimeStreamDemand> {
        match backend {
            LocalizeBackendKind::OrbSlam3Rgbd | LocalizeBackendKind::OrbSlam3RgbdInertial => {
                vec![
                    Self::orb_slam3_camera_demand(),
                    Self::orb_slam3_depth_demand(),
                ]
            }
            LocalizeBackendKind::DeadReckoning | LocalizeBackendKind::GnssAnchored => Vec::new(),
        }
    }

    fn orb_slam3_camera_demand() -> RuntimeStreamDemand {
        RuntimeStreamDemand::Camera(CameraStreamDemand {
            role: Role::Localization.as_str(),
            min_rate_hz: 5.0,
            min_width_px: 320,
            min_height_px: 240,
            accepted_encodings: vec![CameraProfileEncoding::Rgb8, CameraProfileEncoding::Rgba8],
        })
    }

    fn orb_slam3_depth_demand() -> RuntimeStreamDemand {
        RuntimeStreamDemand::Depth(DepthStreamDemand {
            role: Role::Localization.as_str(),
            min_rate_hz: 5.0,
            min_width_px: 320,
            min_height_px: 240,
            accepted_encodings: vec![DepthProfileEncoding::U16Millimeters],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orb_slam3_backend_declares_camera_and_depth_stream_demands() {
        let demands = LocalizeStreamDemands::for_backend(LocalizeBackendKind::OrbSlam3RgbdInertial);

        assert_eq!(
            demands,
            vec![
                LocalizeStreamDemands::orb_slam3_camera_demand(),
                LocalizeStreamDemands::orb_slam3_depth_demand(),
            ]
        );
    }

    #[test]
    fn non_orb_backends_declare_no_stream_demands() {
        for backend in [
            LocalizeBackendKind::DeadReckoning,
            LocalizeBackendKind::GnssAnchored,
        ] {
            assert!(LocalizeStreamDemands::for_backend(backend).is_empty());
        }
    }
}
