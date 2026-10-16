//! Surfaceless EGL + desktop GL 4.3 context for compute passes over dmabufs.

use std::ffi::c_void;
use std::os::fd::RawFd;

use anyhow::{Context, Result, anyhow, bail};
use glow::HasContext;
use khronos_egl as egl;

const EGL_PLATFORM_SURFACELESS_MESA: egl::Enum = 0x31dd;
const EGL_LINUX_DMA_BUF_EXT: egl::Enum = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: egl::Attrib = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: egl::Attrib = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: egl::Attrib = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: egl::Attrib = 0x3274;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: egl::Attrib = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: egl::Attrib = 0x3444;
const EGL_CONTEXT_OPENGL_PROFILE_MASK: egl::Int = 0x30fd;
const EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT: egl::Int = 1;

type ImageTargetTexture2D = unsafe extern "system" fn(target: u32, image: *mut c_void);

/// One plane of a dmabuf, as EGL wants it.
pub struct PlaneDesc {
    pub fd: RawFd,
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub offset: u32,
    pub pitch: u32,
    pub modifier: u64,
}

pub struct Gl {
    egl: egl::Instance<egl::Static>,
    dpy: egl::Display,
    ctx: egl::Context,
    pub gl: glow::Context,
    image_target: ImageTargetTexture2D,
}

impl Gl {
    pub fn new() -> Result<Self> {
        let egl = egl::Instance::new(egl::Static);
        // SAFETY: `DEFAULT_DISPLAY` is the null native display, which the surfaceless platform
        // expects, and the attribute list is terminated by `ATTRIB_NONE`.
        let dpy = unsafe {
            egl.get_platform_display(
                EGL_PLATFORM_SURFACELESS_MESA,
                egl::DEFAULT_DISPLAY,
                &[egl::ATTRIB_NONE],
            )
        }
        .context("eglGetPlatformDisplay(surfaceless)")?;
        egl.initialize(dpy)?;
        egl.bind_api(egl::OPENGL_API)?;
        // SAFETY: A null config handle is only passed through to eglCreateContext (no config is
        // requested); it is never dereferenced here.
        let no_config = unsafe { egl::Config::from_ptr(std::ptr::null_mut()) };
        let ctx = egl.create_context(
            dpy,
            no_config,
            None,
            &[
                egl::CONTEXT_MAJOR_VERSION,
                4,
                egl::CONTEXT_MINOR_VERSION,
                3,
                EGL_CONTEXT_OPENGL_PROFILE_MASK,
                EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT,
                egl::NONE,
            ],
        )?;
        egl.make_current(dpy, None, None, Some(ctx))?;
        // SAFETY: The context created above is current on this thread, and the loader returns
        // pointers from eglGetProcAddress (or null for unknown names).
        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                egl.get_proc_address(s)
                    .map_or(std::ptr::null(), |f| f as *const c_void)
            })
        };
        let image_target = egl
            .get_proc_address("glEGLImageTargetTexture2DOES")
            .ok_or_else(|| anyhow!("no glEGLImageTargetTexture2DOES"))?;
        // SAFETY: `image_target` is the non-null pointer eglGetProcAddress returned for
        // glEGLImageTargetTexture2DOES, whose C signature is `void (GLenum, GLeglImageOES)`,
        // matching `ImageTargetTexture2D`.
        let image_target: ImageTargetTexture2D = unsafe { std::mem::transmute(image_target) };
        let gl = Self {
            egl,
            dpy,
            ctx,
            gl,
            image_target,
        };
        gl.release()?;
        Ok(gl)
    }

    /// Made current per use so the owner may move between threads.
    pub fn make_current(&self) -> Result<()> {
        Ok(self
            .egl
            .make_current(self.dpy, None, None, Some(self.ctx))?)
    }

    pub fn release(&self) -> Result<()> {
        Ok(self.egl.make_current(self.dpy, None, None, None)?)
    }

    pub fn program(&self, src: &str) -> Result<glow::Program> {
        let gl = &self.gl;
        // SAFETY: Callers make this context current on the calling thread first, and every
        // handle used here was created from `self.gl`.
        unsafe {
            let shader = gl
                .create_shader(glow::COMPUTE_SHADER)
                .map_err(|e| anyhow!(e))?;
            gl.shader_source(shader, src);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                bail!("compute shader: {}", gl.get_shader_info_log(shader));
            }
            let program = gl.create_program().map_err(|e| anyhow!(e))?;
            gl.attach_shader(program, shader);
            gl.link_program(program);
            gl.delete_shader(shader);
            if !gl.get_program_link_status(program) {
                bail!("compute program: {}", gl.get_program_info_log(program));
            }
            Ok(program)
        }
    }

    /// Wraps one dmabuf plane as a 2D texture (current context required).
    pub fn import(&self, p: &PlaneDesc) -> Result<(egl::Image, glow::Texture)> {
        let attribs = [
            egl::WIDTH as egl::Attrib,
            p.width as egl::Attrib,
            egl::HEIGHT as egl::Attrib,
            p.height as egl::Attrib,
            EGL_LINUX_DRM_FOURCC_EXT,
            p.fourcc as egl::Attrib,
            EGL_DMA_BUF_PLANE0_FD_EXT,
            p.fd as egl::Attrib,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT,
            p.offset as egl::Attrib,
            EGL_DMA_BUF_PLANE0_PITCH_EXT,
            p.pitch as egl::Attrib,
            EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
            (p.modifier & 0xffff_ffff) as egl::Attrib,
            EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
            (p.modifier >> 32) as egl::Attrib,
            egl::ATTRIB_NONE,
        ];
        let image = self
            .egl
            .create_image(
                self.dpy,
                // SAFETY: EGL_LINUX_DMA_BUF_EXT images take EGL_NO_CONTEXT; the handle is only
                // passed through, never dereferenced.
                unsafe { egl::Context::from_ptr(egl::NO_CONTEXT) },
                EGL_LINUX_DMA_BUF_EXT,
                // SAFETY: EGL_LINUX_DMA_BUF_EXT images take a null client buffer; the handle is
                // only passed through, never dereferenced.
                unsafe { egl::ClientBuffer::from_ptr(std::ptr::null_mut()) },
                &attribs,
            )
            .context("eglCreateImage(dmabuf)")?;
        let gl = &self.gl;
        // SAFETY: The context is current on this thread (callers of `import` make it current),
        // and `image` is a live EGLImage created on `self.dpy` just above, so passing it to the
        // resolved glEGLImageTargetTexture2DOES is valid.
        let tex = unsafe {
            let t = gl.create_texture().map_err(|e| anyhow!(e))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(t));
            (self.image_target)(glow::TEXTURE_2D, image.as_ptr());
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::NEAREST as i32,
            );
            t
        };
        Ok((image, tex))
    }

    /// Frees an imported texture (current context required).
    pub fn forget(&self, (image, tex): (egl::Image, glow::Texture)) {
        // SAFETY: The caller has this context current (see the doc comment), and `tex` was
        // created by `import` on this context and is deleted only here.
        unsafe { self.gl.delete_texture(tex) };
        let _ = self.egl.destroy_image(self.dpy, image);
    }
}

impl Drop for Gl {
    fn drop(&mut self) {
        let _ = self.egl.make_current(self.dpy, None, None, None);
        let _ = self.egl.destroy_context(self.dpy, self.ctx);
    }
}
