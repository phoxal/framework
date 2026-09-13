//! Bounded camera and viewport capture from private native workspaces.

use super::{Workspace, renderer::CameraRenderer};
use crate::{CameraHandle, WorkspaceError};
use mujoco_rs::wrappers::mj_visualization::MjvCamera;

// RGB + depth storage is at most 28 MiB per renderer, before the copied result.
const MAX_PIXELS: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct RendererState {
    renderer: CameraRenderer,
    pub(super) resolution: [usize; 2],
}

/// A free camera expressed in MuJoCo's world frame and degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewCamera {
    /// Point at the center of the viewport.
    pub look_at: [f64; 3],
    /// Distance from the center in meters.
    pub distance: f64,
    /// Horizontal orbit in degrees.
    pub azimuth: f64,
    /// Vertical orbit in degrees.
    pub elevation: f64,
}

/// Copied, top-to-bottom RGB8 pixels and optical-axis depth in meters.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderedCamera {
    resolution: [usize; 2],
    rgb: Box<[u8]>,
    depth_m: Box<[f32]>,
}

impl RenderedCamera {
    /// Returns `[width, height]` in pixels.
    #[must_use]
    pub const fn resolution(&self) -> [usize; 2] {
        self.resolution
    }
    /// Returns row-major RGB8 bytes.
    #[must_use]
    pub fn rgb(&self) -> &[u8] {
        &self.rgb
    }
    /// Returns row-major optical-axis depth in meters; zero marks no surface hit.
    #[must_use]
    pub fn depth_m(&self) -> &[f32] {
        &self.depth_m
    }
}

impl Workspace {
    /// Returns the source-authored offscreen framebuffer dimensions.
    #[must_use]
    pub fn framebuffer_resolution(&self) -> [usize; 2] {
        let model = self.model.inner_arc();
        [
            model.vis().global.offwidth.max(0) as usize,
            model.vis().global.offheight.max(0) as usize,
        ]
    }

    /// Returns a free camera framing the immutable model's extent.
    #[must_use]
    pub fn default_view_camera(&self) -> ViewCamera {
        let model = self.model.inner_arc();
        let camera = MjvCamera::new_free(&model);
        ViewCamera {
            look_at: camera.lookat,
            distance: camera.distance,
            azimuth: camera.azimuth,
            elevation: camera.elevation,
        }
    }

    /// Renders a model-authored camera from this workspace's current state.
    pub fn render_camera(
        &mut self,
        camera: CameraHandle,
    ) -> Result<RenderedCamera, WorkspaceError> {
        if camera.model_identity() != self.model.identity() {
            return Err(render_error("camera belongs to another model"));
        }
        let info = self
            .model
            .camera_info(camera)
            .map_err(|e| render_error(&e.to_string()))?;
        self.render_native(MjvCamera::new_fixed(camera.index()), info.resolution)
    }

    /// Renders a free camera without altering physics or source-authored cameras.
    ///
    /// Dimensions must fit the model's offscreen framebuffer and the finite pixel budget.
    pub fn render_viewport(
        &mut self,
        view: ViewCamera,
        resolution: [usize; 2],
    ) -> Result<RenderedCamera, WorkspaceError> {
        if view
            .look_at
            .iter()
            .chain([view.distance, view.azimuth, view.elevation].iter())
            .any(|v| !v.is_finite())
            || view.distance <= 0.0
        {
            return Err(render_error(
                "viewport camera must have finite coordinates and positive distance",
            ));
        }
        let mut camera = MjvCamera::new_free(&self.model.inner_arc());
        camera.lookat = view.look_at;
        camera.distance = view.distance;
        camera.azimuth = view.azimuth;
        camera.elevation = view.elevation;
        self.render_native(camera, resolution)
    }

    fn render_native(
        &mut self,
        camera: MjvCamera,
        resolution: [usize; 2],
    ) -> Result<RenderedCamera, WorkspaceError> {
        self.ensure_finite_state()?;
        let [width, height] = resolution;
        let pixels = width
            .checked_mul(height)
            .filter(|n| *n > 0 && *n <= MAX_PIXELS)
            .ok_or_else(|| render_error("camera exceeds the finite pixel budget"))?;
        let model = self.model.inner_arc();
        let global = &model.vis().global;
        let framebuffer_pixels = usize::try_from(global.offwidth).ok().and_then(|w| {
            usize::try_from(global.offheight)
                .ok()
                .and_then(|h| w.checked_mul(h))
        });
        if framebuffer_pixels.is_none_or(|n| n == 0 || n > MAX_PIXELS) {
            return Err(render_error(
                "model framebuffer exceeds the finite pixel budget",
            ));
        }
        if width > global.offwidth as usize || height > global.offheight as usize {
            return Err(render_error(
                "camera exceeds the model framebuffer dimensions",
            ));
        }
        if let Some(state) = &mut self.renderer {
            if state.resolution != resolution {
                // Cameras share one native framebuffer/context within this workspace.
                // Only the bounded CPU readback arrays change dimensions.
                state
                    .renderer
                    .resize(width, height)
                    .map_err(|e| render_error(&e))?;
                state.resolution = resolution;
            }
        } else {
            let renderer = CameraRenderer::new(&model, width, height, camera.clone())
                .map_err(|e| render_error(&e))?;
            self.renderer = Some(RendererState {
                renderer,
                resolution,
            });
        }
        let renderer = &mut self
            .renderer
            .as_mut()
            .ok_or_else(|| render_error("renderer was not initialized"))?
            .renderer;
        renderer.set_camera(camera);
        renderer
            .sync_data(&mut self.data)
            .map_err(|e| render_error(&e))?;
        renderer.render().map_err(|e| render_error(&e))?;
        let rgb = renderer.rgb_flat();
        let depth = renderer.depth_flat();
        if rgb.len() != pixels * 3
            || depth.len() != pixels
            || depth.iter().any(|d| !d.is_finite() || *d < 0.0)
        {
            return Err(render_error("renderer returned invalid pixels or depth"));
        }
        Ok(RenderedCamera {
            resolution,
            rgb: rgb.into(),
            depth_m: depth.into(),
        })
    }
}

fn render_error(message: &str) -> WorkspaceError {
    WorkspaceError::Native {
        operation: "render camera",
        message: message.into(),
    }
}
