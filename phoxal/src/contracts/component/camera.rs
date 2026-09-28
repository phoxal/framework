//! The standard visual vocabulary.
//!
//! Producer-independent identities under `phoxal.component.camera.v1`:
//! camera-bearing drivers (for example OAK-D Lite) and the native
//! simulator publish these exact shapes for mono/RGB frames and depth
//! frames.

#[phoxal::messages(package = "phoxal.component.camera.v1")]
mod v1 {
    /// The pixel encoding of one camera frame.
    pub enum ImageEncoding {
        Unspecified = 0,
        Mono8 = 1,
        Rgb8 = 2,
    }

    /// One camera frame: dimensions, pixel encoding, and row-major pixel data.
    pub struct CameraFrame {
        #[phoxal(tag = 1)]
        pub width_px: u32,
        #[phoxal(tag = 2)]
        pub height_px: u32,
        #[phoxal(tag = 3)]
        pub encoding: ImageEncoding,
        #[phoxal(tag = 4)]
        pub data: Vec<u8>,
    }

    /// One depth frame: dimensions and row-major per-pixel depth in
    /// millimetres.
    pub struct DepthFrame {
        #[phoxal(tag = 1)]
        pub width_px: u32,
        #[phoxal(tag = 2)]
        pub height_px: u32,
        #[phoxal(tag = 3)]
        pub depth_mm: Vec<u32>,
    }
}

pub use v1::*;

#[cfg(test)]
mod tests {
    use prost::Name;

    use super::{CameraFrame, DepthFrame};
    use crate::schema::MessageSchema;

    #[test]
    fn retains_public_protobuf_identity_and_schema() {
        assert_eq!(CameraFrame::PACKAGE, "phoxal.component.camera.v1");
        assert_eq!(
            CameraFrame::WIRE_NAME,
            "phoxal.component.camera.v1.CameraFrame"
        );
        assert!(CameraFrame::retain_schema() > 0);
        assert!(DepthFrame::retain_schema() > 0);
    }
}
