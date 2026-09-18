//! RGB and metric depth rendering without a window or application event loop.

#[cfg(target_os = "macos")]
use super::cgl::Context;
#[cfg(not(target_os = "macos"))]
use super::egl::Context;
use mujoco_rs::wrappers::mj_model::traits::ModelType;
use mujoco_rs::{
    prelude::MjData,
    wrappers::{
        MjModel,
        mj_rendering::{MjrContext, MjrRectangle},
        mj_visualization::{MjvCamera, MjvOption, MjvPerturb, MjvScene},
    },
};

#[derive(Debug)]
pub(super) struct CameraRenderer {
    // Explicitly destroyed while gl is current, before releasing the CGL context.
    context: Option<MjrContext>,
    gl: Context,
    scene: MjvScene,
    camera: MjvCamera,
    width: usize,
    height: usize,
    rgb: Vec<u8>,
    depth: Vec<f32>,
    near: f64,
    far: f64,
}

impl CameraRenderer {
    pub(super) fn new(
        model: &MjModel,
        width: usize,
        height: usize,
        camera: MjvCamera,
    ) -> Result<Self, String> {
        if width == 0
            || height == 0
            || width > model.vis().global.offwidth as usize
            || height > model.vis().global.offheight as usize
        {
            return Err("camera dimensions exceed the model's offscreen framebuffer".into());
        }
        let pixels = width
            .checked_mul(height)
            .ok_or("camera dimensions overflow")?;
        let rgb_bytes = pixels.checked_mul(3).ok_or("camera byte count overflows")?;
        let gl = Context::new()?;
        let context = {
            let _current = gl.enter()?;
            // SAFETY: the windowless GL context is current. Every rendering call and
            // context destruction below reinstates it on this same thread.
            let mut context = unsafe { MjrContext::new(model) };
            context.offscreen();
            context
        };
        let scene = MjvScene::new(model, model.ngeom() as usize + 100);
        let near = f64::from(model.vis().map.znear) * model.stat().extent;
        let far = f64::from(model.vis().map.zfar) * model.stat().extent;
        Ok(Self {
            context: Some(context),
            gl,
            scene,
            camera,
            width,
            height,
            rgb: vec![0; rgb_bytes],
            depth: vec![0.0; pixels],
            near,
            far,
        })
    }

    pub(super) fn resize(&mut self, width: usize, height: usize) -> Result<(), String> {
        let pixels = width
            .checked_mul(height)
            .ok_or("camera dimensions overflow")?;
        let bytes = pixels.checked_mul(3).ok_or("camera byte count overflows")?;
        self.rgb.resize(bytes, 0);
        self.depth.resize(pixels, 0.0);
        self.width = width;
        self.height = height;
        Ok(())
    }

    pub(super) fn set_camera(&mut self, camera: MjvCamera) {
        self.camera = camera;
    }

    pub(super) fn sync_data<M: ModelType>(&mut self, data: &mut MjData<M>) -> Result<(), String> {
        if !self.scene.is_compatible_with_model(data.model()) {
            return Err("renderer model changed".into());
        }
        self.scene.update(
            data,
            &MjvOption::default(),
            &MjvPerturb::default(),
            &mut self.camera,
        );
        Ok(())
    }

    pub(super) fn render(&mut self) -> Result<(), String> {
        let _current = self.gl.enter()?;
        let context = self
            .context
            .as_ref()
            .ok_or("rendering context has been destroyed")?;
        let viewport = MjrRectangle::new(0, 0, self.width as i32, self.height as i32);
        self.scene.render(&viewport, context);
        context
            .read_pixels(Some(&mut self.rgb), Some(&mut self.depth), &viewport)
            .map_err(|e| e.to_string())?;
        // OpenGL returns bottom-up pixels and a non-linear depth buffer.
        for row in 0..self.height / 2 {
            let opposite = self.height - row - 1;
            for column in 0..self.width {
                self.depth
                    .swap(row * self.width + column, opposite * self.width + column);
                for channel in 0..3 {
                    self.rgb.swap(
                        (row * self.width + column) * 3 + channel,
                        (opposite * self.width + column) * 3 + channel,
                    );
                }
            }
        }
        for depth in &mut self.depth {
            *depth = if *depth >= 1.0 {
                0.0
            } else {
                (self.near / (1.0 - f64::from(*depth) * (1.0 - self.near / self.far))) as f32
            };
        }
        Ok(())
    }

    pub(super) fn rgb_flat(&self) -> &[u8] {
        &self.rgb
    }
    pub(super) fn depth_flat(&self) -> &[f32] {
        &self.depth
    }
}

impl Drop for CameraRenderer {
    fn drop(&mut self) {
        match self.gl.enter() {
            Ok(_current) => {
                drop(self.context.take());
            }
            // A lost OS context cannot safely free GL resources. Keep the native
            // allocation alive instead of invoking MuJoCo with no current context.
            Err(_) => {
                if let Some(context) = self.context.take() {
                    std::mem::forget(context);
                }
            }
        }
    }
}
