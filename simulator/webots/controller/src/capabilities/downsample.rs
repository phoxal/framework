// Step 2a intentionally lands pure, tested primitives before step 2b wires
// subscriber-requested specs into the Webots capture loop.
#![allow(dead_code)]

use anyhow::{Result, anyhow};
use phoxal::api::v1::component::capability::{
    camera::{self, Encoding as CameraEncoding},
    depth,
    profile::{CameraProfileEncoding, CameraProfileSpec, DepthProfileSpec},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CropRect {
    pub x_px: u32,
    pub y_px: u32,
    pub width_px: u32,
    pub height_px: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RateDecimation {
    publish_every_n: u64,
}

impl RateDecimation {
    pub(crate) fn new(native_rate_hz: f64, target_rate_hz: f64) -> Result<Self> {
        if !native_rate_hz.is_finite() || native_rate_hz <= f64::EPSILON {
            anyhow::bail!("native_rate_hz must be > 0");
        }
        if !target_rate_hz.is_finite() || target_rate_hz <= f64::EPSILON {
            anyhow::bail!("target_rate_hz must be > 0");
        }
        if target_rate_hz > native_rate_hz {
            anyhow::bail!(
                "target_rate_hz ({target_rate_hz}) must not exceed native_rate_hz ({native_rate_hz})"
            );
        }

        let publish_every_n = (native_rate_hz / target_rate_hz).round().max(1.0) as u64;
        Ok(Self { publish_every_n })
    }

    pub(crate) const fn publish_every_n(&self) -> u64 {
        self.publish_every_n
    }

    pub(crate) const fn should_emit(&self, native_frame_index: u64) -> bool {
        native_frame_index.is_multiple_of(self.publish_every_n)
    }
}

pub(crate) fn downsample_camera_frame(
    native: &camera::Frame,
    target: &CameraProfileSpec,
    crop: Option<CropRect>,
) -> Result<camera::Frame> {
    let target_encoding = camera_encoding(target.encoding.clone())?;
    validate_target_bounds(
        native.width(),
        native.height(),
        target.width_px,
        target.height_px,
    )?;
    let crop = crop.unwrap_or(CropRect {
        x_px: 0,
        y_px: 0,
        width_px: native.width(),
        height_px: native.height(),
    });
    validate_crop(native.width(), native.height(), crop)?;
    validate_target_bounds(
        crop.width_px,
        crop.height_px,
        target.width_px,
        target.height_px,
    )?;

    let bytes_per_pixel = bytes_per_camera_pixel(native.encoding())?;
    validate_camera_data_len(native, bytes_per_pixel)?;

    // Nearest-neighbor keeps the simulator preview deterministic and cheap; the
    // driver only promises raw source reduction, not production image quality.
    let resized = resize_camera_nearest(
        native.data(),
        native.width(),
        crop,
        bytes_per_pixel,
        target.width_px,
        target.height_px,
    );
    let data = convert_camera_encoding(
        &resized,
        native.encoding(),
        target_encoding,
        target.width_px,
        target.height_px,
    )?;

    Ok(camera::Frame::new(
        target.width_px,
        target.height_px,
        target_encoding,
        data,
    ))
}

pub(crate) fn downsample_depth_frame(
    native: &depth::Depth,
    native_width_px: u32,
    native_height_px: u32,
    target: &DepthProfileSpec,
    crop: Option<CropRect>,
) -> Result<depth::Depth> {
    validate_target_bounds(
        native_width_px,
        native_height_px,
        target.width_px,
        target.height_px,
    )?;
    validate_depth_data_len(native, native_width_px, native_height_px)?;
    let crop = crop.unwrap_or(CropRect {
        x_px: 0,
        y_px: 0,
        width_px: native_width_px,
        height_px: native_height_px,
    });
    validate_crop(native_width_px, native_height_px, crop)?;
    validate_target_bounds(
        crop.width_px,
        crop.height_px,
        target.width_px,
        target.height_px,
    )?;

    // Depth uses nearest-neighbor so invalid sentinel samples are preserved
    // exactly instead of being blended with neighboring valid samples.
    let samples_mm = resize_depth_nearest(
        native.samples_mm(),
        native_width_px,
        crop,
        target.width_px,
        target.height_px,
    );
    Ok(depth::Depth::new(samples_mm))
}

fn validate_target_bounds(
    source_width_px: u32,
    source_height_px: u32,
    target_width_px: u32,
    target_height_px: u32,
) -> Result<()> {
    if target_width_px == 0 || target_height_px == 0 {
        anyhow::bail!("target dimensions must be > 0");
    }
    if target_width_px > source_width_px || target_height_px > source_height_px {
        anyhow::bail!(
            "target dimensions {target_width_px}x{target_height_px} exceed source dimensions {source_width_px}x{source_height_px}"
        );
    }
    Ok(())
}

fn validate_crop(native_width_px: u32, native_height_px: u32, crop: CropRect) -> Result<()> {
    if crop.width_px == 0 || crop.height_px == 0 {
        anyhow::bail!("crop dimensions must be > 0");
    }
    let crop_right = crop
        .x_px
        .checked_add(crop.width_px)
        .ok_or_else(|| anyhow!("crop x range overflows"))?;
    let crop_bottom = crop
        .y_px
        .checked_add(crop.height_px)
        .ok_or_else(|| anyhow!("crop y range overflows"))?;
    if crop_right > native_width_px || crop_bottom > native_height_px {
        anyhow::bail!(
            "crop {}x{} at {},{} exceeds native dimensions {native_width_px}x{native_height_px}",
            crop.width_px,
            crop.height_px,
            crop.x_px,
            crop.y_px
        );
    }
    Ok(())
}

fn camera_encoding(encoding: CameraProfileEncoding) -> Result<CameraEncoding> {
    match encoding {
        CameraProfileEncoding::Rgb8 => Ok(CameraEncoding::Rgb8),
        CameraProfileEncoding::Rgba8 => Ok(CameraEncoding::Rgba8),
        CameraProfileEncoding::L8 | CameraProfileEncoding::Jpeg | CameraProfileEncoding::Png => {
            anyhow::bail!("driver downsampling only supports rgb8 and rgba8 camera targets")
        }
    }
}

fn bytes_per_camera_pixel(encoding: CameraEncoding) -> Result<usize> {
    match encoding {
        CameraEncoding::Rgb8 => Ok(3),
        CameraEncoding::Rgba8 => Ok(4),
        CameraEncoding::Jpeg | CameraEncoding::Png | CameraEncoding::L8 => {
            anyhow::bail!("driver downsampling only supports rgb8 and rgba8 native frames")
        }
    }
}

fn validate_camera_data_len(native: &camera::Frame, bytes_per_pixel: usize) -> Result<()> {
    let expected = pixel_count(native.width(), native.height())?
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| anyhow!("camera byte count overflows"))?;
    if native.data().len() != expected {
        anyhow::bail!(
            "camera frame has {} bytes; expected {expected}",
            native.data().len()
        );
    }
    Ok(())
}

fn validate_depth_data_len(
    native: &depth::Depth,
    native_width_px: u32,
    native_height_px: u32,
) -> Result<()> {
    let expected = pixel_count(native_width_px, native_height_px)?;
    if native.samples_mm().len() != expected {
        anyhow::bail!(
            "depth frame has {} samples; expected {expected}",
            native.samples_mm().len()
        );
    }
    Ok(())
}

fn pixel_count(width_px: u32, height_px: u32) -> Result<usize> {
    let pixels = u64::from(width_px)
        .checked_mul(u64::from(height_px))
        .ok_or_else(|| anyhow!("pixel count overflows"))?;
    usize::try_from(pixels).map_err(|_| anyhow!("pixel count exceeds usize"))
}

fn resize_camera_nearest(
    data: &[u8],
    native_width_px: u32,
    crop: CropRect,
    bytes_per_pixel: usize,
    target_width_px: u32,
    target_height_px: u32,
) -> Vec<u8> {
    let target_len = target_width_px as usize * target_height_px as usize * bytes_per_pixel;
    let mut resized = Vec::with_capacity(target_len);
    for target_y in 0..target_height_px {
        let source_y = crop.y_px + nearest_source_index(target_y, crop.height_px, target_height_px);
        for target_x in 0..target_width_px {
            let source_x =
                crop.x_px + nearest_source_index(target_x, crop.width_px, target_width_px);
            let source_index = ((source_y * native_width_px + source_x) as usize) * bytes_per_pixel;
            resized.extend_from_slice(&data[source_index..source_index + bytes_per_pixel]);
        }
    }
    resized
}

fn resize_depth_nearest(
    samples_mm: &[u16],
    native_width_px: u32,
    crop: CropRect,
    target_width_px: u32,
    target_height_px: u32,
) -> Vec<u16> {
    let mut resized = Vec::with_capacity(target_width_px as usize * target_height_px as usize);
    for target_y in 0..target_height_px {
        let source_y = crop.y_px + nearest_source_index(target_y, crop.height_px, target_height_px);
        for target_x in 0..target_width_px {
            let source_x =
                crop.x_px + nearest_source_index(target_x, crop.width_px, target_width_px);
            let source_index = (source_y * native_width_px + source_x) as usize;
            resized.push(samples_mm[source_index]);
        }
    }
    resized
}

fn nearest_source_index(target_index: u32, source_len: u32, target_len: u32) -> u32 {
    (u64::from(target_index) * u64::from(source_len) / u64::from(target_len)) as u32
}

fn convert_camera_encoding(
    data: &[u8],
    source_encoding: CameraEncoding,
    target_encoding: CameraEncoding,
    target_width_px: u32,
    target_height_px: u32,
) -> Result<Vec<u8>> {
    match (source_encoding, target_encoding) {
        (CameraEncoding::Rgb8, CameraEncoding::Rgb8)
        | (CameraEncoding::Rgba8, CameraEncoding::Rgba8) => Ok(data.to_vec()),
        (CameraEncoding::Rgba8, CameraEncoding::Rgb8) => Ok(data
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect()),
        (CameraEncoding::Rgb8, CameraEncoding::Rgba8) => {
            let mut rgba = Vec::with_capacity(pixel_count(target_width_px, target_height_px)? * 4);
            for pixel in data.chunks_exact(3) {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], u8::MAX]);
            }
            Ok(rgba)
        }
        _ => anyhow::bail!("driver downsampling only supports rgb8 and rgba8 conversion"),
    }
}

#[cfg(test)]
mod tests {
    use super::{CropRect, RateDecimation, downsample_camera_frame, downsample_depth_frame};
    use phoxal::api::v1::component::capability::{
        camera::{Encoding as CameraEncoding, Frame as CameraFrame},
        depth::Depth,
        profile::{CameraProfileEncoding, CameraProfileSpec, DepthProfileSpec},
    };

    #[test]
    fn camera_downsamples_to_target_resolution_and_rgb8() {
        let native = CameraFrame::new(640, 480, CameraEncoding::Rgba8, rgba_image(640, 480));
        let target = CameraProfileSpec {
            width_px: 320,
            height_px: 240,
            publish_rate_hz: 15.0,
            encoding: CameraProfileEncoding::Rgb8,
        };

        let frame = downsample_camera_frame(&native, &target, None).unwrap();

        assert_eq!(frame.width(), 320);
        assert_eq!(frame.height(), 240);
        assert_eq!(frame.encoding(), CameraEncoding::Rgb8);
        assert_eq!(frame.data().len(), 320 * 240 * 3);
        assert_eq!(&frame.data()[0..3], &[0, 0, 0]);
        assert_eq!(&frame.data()[3..6], &[2, 0, 2]);
    }

    #[test]
    fn camera_crop_to_centered_roi_yields_roi_pixels() {
        let native = CameraFrame::new(640, 480, CameraEncoding::Rgba8, rgba_image(640, 480));
        let target = CameraProfileSpec {
            width_px: 320,
            height_px: 240,
            publish_rate_hz: 15.0,
            encoding: CameraProfileEncoding::Rgba8,
        };

        let frame = downsample_camera_frame(
            &native,
            &target,
            Some(CropRect {
                x_px: 160,
                y_px: 120,
                width_px: 320,
                height_px: 240,
            }),
        )
        .unwrap();

        assert_eq!(&frame.data()[0..4], &rgba_pixel(160, 120));
        let last_pixel_start = frame.data().len() - 4;
        assert_eq!(&frame.data()[last_pixel_start..], &rgba_pixel(479, 359));
    }

    #[test]
    fn camera_converts_rgb8_to_rgba8() {
        let native = CameraFrame::new(2, 1, CameraEncoding::Rgb8, [1, 2, 3, 4, 5, 6]);
        let target = CameraProfileSpec {
            width_px: 2,
            height_px: 1,
            publish_rate_hz: 30.0,
            encoding: CameraProfileEncoding::Rgba8,
        };

        let frame = downsample_camera_frame(&native, &target, None).unwrap();

        assert_eq!(frame.encoding(), CameraEncoding::Rgba8);
        assert_eq!(frame.data(), &[1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn depth_downsamples_with_nearest_neighbor_without_blending_invalid_samples() {
        let mut samples = vec![1_000_u16; 640 * 480];
        samples[0] = 0;
        samples[1] = 2_000;
        samples[640] = 3_000;
        samples[641] = 4_000;
        let native = Depth::new(samples);
        let target = DepthProfileSpec {
            width_px: 320,
            height_px: 240,
            publish_rate_hz: 15.0,
        };

        let depth = downsample_depth_frame(&native, 640, 480, &target, None).unwrap();

        assert_eq!(depth.samples_mm().len(), 320 * 240);
        assert_eq!(depth.samples_mm()[0], 0);
        assert_eq!(depth.samples_mm()[1], 1_000);
    }

    #[test]
    fn rate_decimation_emits_every_second_frame_for_100hz_to_50hz() {
        let decimation = RateDecimation::new(100.0, 50.0).unwrap();

        assert_eq!(decimation.publish_every_n(), 2);
        assert!(decimation.should_emit(0));
        assert!(!decimation.should_emit(1));
        assert!(decimation.should_emit(2));
        assert!(!decimation.should_emit(3));
    }

    #[test]
    fn rate_decimation_emits_every_frame_for_native_rate() {
        let decimation = RateDecimation::new(100.0, 100.0).unwrap();

        assert_eq!(decimation.publish_every_n(), 1);
        assert!(decimation.should_emit(0));
        assert!(decimation.should_emit(1));
    }

    #[test]
    fn rate_decimation_rejects_target_rate_above_native_rate() {
        assert!(RateDecimation::new(100.0, 101.0).is_err());
    }

    #[test]
    fn camera_rejects_crop_and_resize_exceeding_native_bounds() {
        let native = CameraFrame::new(640, 480, CameraEncoding::Rgba8, rgba_image(640, 480));
        let oversized_target = CameraProfileSpec {
            width_px: 641,
            height_px: 480,
            publish_rate_hz: 15.0,
            encoding: CameraProfileEncoding::Rgb8,
        };
        let valid_target = CameraProfileSpec {
            width_px: 320,
            height_px: 240,
            publish_rate_hz: 15.0,
            encoding: CameraProfileEncoding::Rgb8,
        };

        assert!(downsample_camera_frame(&native, &oversized_target, None).is_err());
        assert!(
            downsample_camera_frame(
                &native,
                &valid_target,
                Some(CropRect {
                    x_px: 400,
                    y_px: 120,
                    width_px: 320,
                    height_px: 240,
                }),
            )
            .is_err()
        );
    }

    #[test]
    fn camera_rejects_resize_that_exceeds_crop_bounds() {
        let native = CameraFrame::new(640, 480, CameraEncoding::Rgba8, rgba_image(640, 480));
        let target = CameraProfileSpec {
            width_px: 320,
            height_px: 240,
            publish_rate_hz: 15.0,
            encoding: CameraProfileEncoding::Rgb8,
        };

        assert!(
            downsample_camera_frame(
                &native,
                &target,
                Some(CropRect {
                    x_px: 160,
                    y_px: 120,
                    width_px: 160,
                    height_px: 120,
                }),
            )
            .is_err()
        );
    }

    #[test]
    fn depth_rejects_crop_and_resize_exceeding_native_bounds() {
        let native = Depth::new(vec![1_000; 640 * 480]);
        let oversized_target = DepthProfileSpec {
            width_px: 320,
            height_px: 481,
            publish_rate_hz: 15.0,
        };
        let valid_target = DepthProfileSpec {
            width_px: 320,
            height_px: 240,
            publish_rate_hz: 15.0,
        };

        assert!(downsample_depth_frame(&native, 640, 480, &oversized_target, None).is_err());
        assert!(
            downsample_depth_frame(
                &native,
                640,
                480,
                &valid_target,
                Some(CropRect {
                    x_px: 160,
                    y_px: 300,
                    width_px: 320,
                    height_px: 240,
                }),
            )
            .is_err()
        );
    }

    #[test]
    fn depth_rejects_resize_that_exceeds_crop_bounds() {
        let native = Depth::new(vec![1_000; 640 * 480]);
        let target = DepthProfileSpec {
            width_px: 320,
            height_px: 240,
            publish_rate_hz: 15.0,
        };

        assert!(
            downsample_depth_frame(
                &native,
                640,
                480,
                &target,
                Some(CropRect {
                    x_px: 160,
                    y_px: 120,
                    width_px: 160,
                    height_px: 120,
                }),
            )
            .is_err()
        );
    }

    fn rgba_image(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                data.extend_from_slice(&rgba_pixel(x, y));
            }
        }
        data
    }

    fn rgba_pixel(x: u32, y: u32) -> [u8; 4] {
        [x as u8, y as u8, x.wrapping_add(y) as u8, 255]
    }
}
