//! Windowless EGL context for Linux graphics and software-rendering devices.

use glutin::{
    api::egl::{
        context::PossiblyCurrentContext, device::Device, display::Display, surface::Surface,
    },
    config::{ConfigSurfaceTypes, ConfigTemplateBuilder},
    context::{ContextApi, ContextAttributesBuilder, GlProfile, Version},
    prelude::{GlDisplay, NotCurrentGlContext, PossiblyCurrentGlContext},
    surface::{PbufferSurface, SurfaceAttributesBuilder},
};

#[derive(Debug)]
pub(super) struct Context {
    context: PossiblyCurrentContext,
    surface: Surface<PbufferSurface>,
}

impl Context {
    pub(super) fn new() -> Result<Self, String> {
        let devices = Device::query_devices().map_err(|e| e.to_string())?;
        let mut errors = Vec::new();
        for device in devices {
            match Self::on_device(&device) {
                Ok(context) => return Ok(context),
                Err(error) => errors.push(error.to_string()),
            }
        }
        Err(format!(
            "no usable offscreen EGL device: {}",
            errors.join("; ")
        ))
    }

    fn on_device(device: &Device) -> Result<Self, glutin::error::Error> {
        // SAFETY: device was discovered by EGL and remains valid during creation.
        let display = unsafe { Display::with_device(device, None)? };
        let template = ConfigTemplateBuilder::new()
            .with_surface_type(ConfigSurfaceTypes::PBUFFER)
            .with_depth_size(24)
            .build();
        // SAFETY: the display owns the returned configurations.
        let config = unsafe { display.find_configs(template)? }
            .next()
            .ok_or(glutin::error::ErrorKind::NotFound)?;
        let attributes = ContextAttributesBuilder::new()
            .with_profile(GlProfile::Compatibility)
            .with_context_api(ContextApi::OpenGl(Some(Version::new(2, 0))))
            .build(None);
        // SAFETY: config came from display; no window handle or shared context is used.
        let context = unsafe { display.create_context(&config, &attributes)? };
        // MuJoCo uses its own FBO. The EGL drawable only establishes the context.
        let attributes = SurfaceAttributesBuilder::<PbufferSurface>::new()
            .build(std::num::NonZeroU32::MIN, std::num::NonZeroU32::MIN);
        // SAFETY: the compatible configuration and nonzero pbuffer dimensions are valid.
        let surface = unsafe { display.create_pbuffer_surface(&config, &attributes)? };
        let context = context.make_current(&surface)?;
        Ok(Self { context, surface })
    }

    pub(super) fn enter(&self) -> Result<Current<'_>, String> {
        self.context
            .make_current(&self.surface)
            .map_err(|e| e.to_string())?;
        Ok(Current(self))
    }
}

pub(super) struct Current<'a>(&'a Context);
impl Drop for Current<'_> {
    fn drop(&mut self) {
        let _ = self.0.context.make_not_current_in_place();
    }
}
