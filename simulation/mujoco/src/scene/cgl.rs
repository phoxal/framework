//! Thread-owned, windowless macOS context for MuJoCo's compatibility renderer.

use std::{marker::PhantomData, ptr, rc::Rc};

/// CGL contexts are thread-local. This owner cannot move between threads.
#[derive(Debug)]
pub(super) struct Context {
    raw: cgl::CGLContextObj,
    _thread: PhantomData<Rc<()>>,
}

impl Context {
    pub(super) fn new() -> Result<Self, String> {
        // MuJoCo uses fixed-function OpenGL, so CGL's default legacy profile is required.
        let attributes = [
            cgl::kCGLPFAColorSize,
            24,
            cgl::kCGLPFAAlphaSize,
            8,
            cgl::kCGLPFADepthSize,
            24,
            cgl::kCGLPFAStencilSize,
            8,
            cgl::kCGLPFAAccelerated,
            0,
        ];
        let mut format = ptr::null_mut();
        let mut count = 0;
        // SAFETY: the attributes are terminated and both outputs point to valid storage.
        check(unsafe { cgl::CGLChoosePixelFormat(attributes.as_ptr(), &mut format, &mut count) })?;
        if format.is_null() {
            return Err("CGL returned no compatible pixel format".into());
        }
        let mut raw = ptr::null_mut();
        // SAFETY: format is an owned, valid CGL pixel format; no context is shared.
        let status = unsafe { cgl::CGLCreateContext(format, ptr::null_mut(), &mut raw) };
        // SAFETY: creation has finished using the pixel format.
        unsafe { cgl::CGLReleasePixelFormat(format) };
        check(status)?;
        if raw.is_null() {
            return Err("CGL returned no rendering context".into());
        }
        Ok(Self {
            raw,
            _thread: PhantomData,
        })
    }

    /// Restore the caller's context after each operation, including on error.
    pub(super) fn enter(&self) -> Result<Current<'_>, String> {
        // SAFETY: this owner remains on its creation thread and raw is still alive.
        let previous = unsafe { cgl::CGLGetCurrentContext() };
        // SAFETY: raw is a valid context exclusively owned on this thread.
        check(unsafe { cgl::CGLSetCurrentContext(self.raw) })?;
        Ok(Current {
            previous,
            _owner: self,
        })
    }
}

pub(super) struct Current<'a> {
    previous: cgl::CGLContextObj,
    _owner: &'a Context,
}

impl Drop for Current<'_> {
    fn drop(&mut self) {
        // SAFETY: the previous thread-local context was not released by this owner.
        unsafe { cgl::CGLSetCurrentContext(self.previous) };
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: all borrowed current guards have ended, and raw is released exactly once.
        unsafe { cgl::CGLDestroyContext(self.raw) };
    }
}

fn check(status: cgl::CGLError) -> Result<(), String> {
    if status == cgl::kCGLNoError {
        Ok(())
    } else {
        Err(format!("CGL operation failed with code {status}"))
    }
}
