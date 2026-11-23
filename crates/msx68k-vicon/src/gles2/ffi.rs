//! Dynamic FFI bindings to `libEGL.so.1` and `libGLESv2.so.2` via `libloading`.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::ptr;

use libloading::Library;

/// Proc-address loader callback compatible with libretro's `retro_hw_get_proc_address_t`.
pub type GlGetProcAddressFn = unsafe extern "C" fn(*const c_char) -> *const c_void;

pub type GLenum = c_uint;
pub type GLboolean = u8;
pub type GLbitfield = c_uint;
pub type GLint = c_int;
pub type GLsizei = c_int;
pub type GLuint = c_uint;
pub type GLfloat = f32;
pub type GLsizeiptr = isize;
pub type GLintptr = isize;

pub type EGLDisplay = *mut c_void;
pub type EGLConfig = *mut c_void;
pub type EGLContext = *mut c_void;
pub type EGLSurface = *mut c_void;
pub type EGLBoolean = c_uint;
pub type EGLenum = c_uint;
pub type EGLint = c_int;

pub const EGL_FALSE: EGLBoolean = 0;
pub const EGL_NONE: EGLint = 0x3038;
pub const EGL_RED_SIZE: EGLint = 0x3024;
pub const EGL_GREEN_SIZE: EGLint = 0x3023;
pub const EGL_BLUE_SIZE: EGLint = 0x3022;
pub const EGL_ALPHA_SIZE: EGLint = 0x3021;
pub const EGL_SURFACE_TYPE: EGLint = 0x3033;
pub const EGL_PBUFFER_BIT: EGLint = 0x0001;
pub const EGL_RENDERABLE_TYPE: EGLint = 0x3040;
pub const EGL_OPENGL_ES2_BIT: EGLint = 0x0004;
pub const EGL_WIDTH: EGLint = 0x3057;
pub const EGL_HEIGHT: EGLint = 0x3056;
pub const EGL_CONTEXT_CLIENT_VERSION: EGLint = 0x3098;
pub const EGL_OPENGL_ES_API: EGLenum = 0x30A0;
pub const EGL_PLATFORM_SURFACELESS_MESA: EGLenum = 0x31DD;

pub const GL_FALSE: GLboolean = 0;
pub const GL_TRUE: GLboolean = 1;
pub const GL_TRIANGLES: GLenum = 0x0004;
pub const GL_TRIANGLE_STRIP: GLenum = 0x0005;
pub const GL_LESS: GLenum = 0x0201;
pub const GL_UNSIGNED_BYTE: GLenum = 0x1401;
pub const GL_UNSIGNED_SHORT: GLenum = 0x1403;
pub const GL_FLOAT: GLenum = 0x1406;
pub const GL_UNSIGNED_SHORT_5_6_5: GLenum = 0x8363;
pub const GL_RGB: GLenum = 0x1907;
pub const GL_RGBA: GLenum = 0x1908;
pub const GL_LUMINANCE: GLenum = 0x1909;
pub const GL_LUMINANCE_ALPHA: GLenum = 0x190A;
pub const GL_TEXTURE_2D: GLenum = 0x0DE1;
pub const GL_NEAREST: GLint = 0x2600;
pub const GL_LINEAR: GLint = 0x2601;
pub const GL_TEXTURE_MAG_FILTER: GLenum = 0x2800;
pub const GL_TEXTURE_MIN_FILTER: GLenum = 0x2801;
pub const GL_TEXTURE_WRAP_S: GLenum = 0x2802;
pub const GL_TEXTURE_WRAP_T: GLenum = 0x2803;
pub const GL_CLAMP_TO_EDGE: GLint = 0x812F;
pub const GL_DEPTH_TEST: GLenum = 0x0B71;
pub const GL_STENCIL_TEST: GLenum = 0x0B90;
pub const GL_SCISSOR_TEST: GLenum = 0x0C11;
pub const GL_BLEND: GLenum = 0x0BE2;
pub const GL_DITHER: GLenum = 0x0BD0;
pub const GL_CULL_FACE: GLenum = 0x0B44;
pub const GL_DEPTH_BUFFER_BIT: GLbitfield = 0x0000_0100;
pub const GL_COLOR_BUFFER_BIT: GLbitfield = 0x0000_4000;
pub const GL_UNPACK_ALIGNMENT: GLenum = 0x0CF5;
pub const GL_PACK_ALIGNMENT: GLenum = 0x0D05;
pub const GL_MAX_TEXTURE_SIZE: GLenum = 0x0D33;
pub const GL_MAX_TEXTURE_IMAGE_UNITS: GLenum = 0x8872;
pub const GL_IMPLEMENTATION_COLOR_READ_TYPE: GLenum = 0x8B9A;
pub const GL_IMPLEMENTATION_COLOR_READ_FORMAT: GLenum = 0x8B9B;
pub const GL_TEXTURE0: GLenum = 0x84C0;
pub const GL_ARRAY_BUFFER: GLenum = 0x8892;
pub const GL_ELEMENT_ARRAY_BUFFER: GLenum = 0x8893;
pub const GL_STATIC_DRAW: GLenum = 0x88E4;
pub const GL_DYNAMIC_DRAW: GLenum = 0x88E8;
pub const GL_FRAGMENT_SHADER: GLenum = 0x8B30;
pub const GL_VERTEX_SHADER: GLenum = 0x8B31;
pub const GL_COMPILE_STATUS: GLenum = 0x8B81;
pub const GL_LINK_STATUS: GLenum = 0x8B82;
pub const GL_FRAMEBUFFER: GLenum = 0x8D40;
pub const GL_RENDERBUFFER: GLenum = 0x8D41;
pub const GL_DEPTH_COMPONENT16: GLenum = 0x81A5;
pub const GL_RGB565: GLenum = 0x8D62;
pub const GL_COLOR_ATTACHMENT0: GLenum = 0x8CE0;
pub const GL_DEPTH_ATTACHMENT: GLenum = 0x8D00;
pub const GL_FRAMEBUFFER_COMPLETE: GLenum = 0x8CD5;

#[allow(non_snake_case, dead_code)]
pub struct EglProcs {
    pub eglGetDisplay: unsafe extern "C" fn(*mut c_void) -> EGLDisplay,
    pub eglGetPlatformDisplay: Option<
        unsafe extern "C" fn(EGLenum, *mut c_void, *const EGLint) -> EGLDisplay,
    >,
    pub eglInitialize: unsafe extern "C" fn(EGLDisplay, *mut EGLint, *mut EGLint) -> EGLBoolean,
    pub eglTerminate: unsafe extern "C" fn(EGLDisplay) -> EGLBoolean,
    pub eglBindAPI: unsafe extern "C" fn(EGLenum) -> EGLBoolean,
    pub eglChooseConfig: unsafe extern "C" fn(
        EGLDisplay,
        *const EGLint,
        *mut EGLConfig,
        EGLint,
        *mut EGLint,
    ) -> EGLBoolean,
    pub eglCreatePbufferSurface:
        unsafe extern "C" fn(EGLDisplay, EGLConfig, *const EGLint) -> EGLSurface,
    pub eglDestroySurface: unsafe extern "C" fn(EGLDisplay, EGLSurface) -> EGLBoolean,
    pub eglCreateContext:
        unsafe extern "C" fn(EGLDisplay, EGLConfig, EGLContext, *const EGLint) -> EGLContext,
    pub eglDestroyContext: unsafe extern "C" fn(EGLDisplay, EGLContext) -> EGLBoolean,
    pub eglMakeCurrent:
        unsafe extern "C" fn(EGLDisplay, EGLSurface, EGLSurface, EGLContext) -> EGLBoolean,
    pub eglGetCurrentContext: unsafe extern "C" fn() -> EGLContext,
    pub eglGetProcAddress: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
}

#[allow(non_snake_case, dead_code)]
pub struct Gles2Procs {
    pub glActiveTexture: unsafe extern "C" fn(GLenum),
    pub glAttachShader: unsafe extern "C" fn(GLuint, GLuint),
    pub glBindAttribLocation: unsafe extern "C" fn(GLuint, GLuint, *const c_char),
    pub glBindBuffer: unsafe extern "C" fn(GLenum, GLuint),
    pub glBindFramebuffer: unsafe extern "C" fn(GLenum, GLuint),
    pub glBindRenderbuffer: unsafe extern "C" fn(GLenum, GLuint),
    pub glBindTexture: unsafe extern "C" fn(GLenum, GLuint),
    pub glBufferData: unsafe extern "C" fn(GLenum, GLsizeiptr, *const c_void, GLenum),
    pub glBufferSubData: unsafe extern "C" fn(GLenum, GLintptr, GLsizeiptr, *const c_void),
    pub glCheckFramebufferStatus: unsafe extern "C" fn(GLenum) -> GLenum,
    pub glClear: unsafe extern "C" fn(GLbitfield),
    pub glClearColor: unsafe extern "C" fn(GLfloat, GLfloat, GLfloat, GLfloat),
    pub glClearDepthf: unsafe extern "C" fn(GLfloat),
    pub glColorMask: unsafe extern "C" fn(GLboolean, GLboolean, GLboolean, GLboolean),
    pub glCompileShader: unsafe extern "C" fn(GLuint),
    pub glCreateProgram: unsafe extern "C" fn() -> GLuint,
    pub glCreateShader: unsafe extern "C" fn(GLenum) -> GLuint,
    pub glDeleteBuffers: unsafe extern "C" fn(GLsizei, *const GLuint),
    pub glDeleteFramebuffers: unsafe extern "C" fn(GLsizei, *const GLuint),
    pub glDeleteProgram: unsafe extern "C" fn(GLuint),
    pub glDeleteRenderbuffers: unsafe extern "C" fn(GLsizei, *const GLuint),
    pub glDeleteShader: unsafe extern "C" fn(GLuint),
    pub glDeleteTextures: unsafe extern "C" fn(GLsizei, *const GLuint),
    pub glDepthFunc: unsafe extern "C" fn(GLenum),
    pub glDepthMask: unsafe extern "C" fn(GLboolean),
    pub glDisable: unsafe extern "C" fn(GLenum),
    pub glDisableVertexAttribArray: unsafe extern "C" fn(GLuint),
    pub glDrawArrays: unsafe extern "C" fn(GLenum, GLint, GLsizei),
    pub glDrawElements: unsafe extern "C" fn(GLenum, GLsizei, GLenum, *const c_void),
    pub glEnable: unsafe extern "C" fn(GLenum),
    pub glEnableVertexAttribArray: unsafe extern "C" fn(GLuint),
    pub glFinish: unsafe extern "C" fn(),
    pub glFlush: unsafe extern "C" fn(),
    pub glFramebufferRenderbuffer: unsafe extern "C" fn(GLenum, GLenum, GLenum, GLuint),
    pub glFramebufferTexture2D: unsafe extern "C" fn(GLenum, GLenum, GLenum, GLuint, GLint),
    pub glGenBuffers: unsafe extern "C" fn(GLsizei, *mut GLuint),
    pub glGenFramebuffers: unsafe extern "C" fn(GLsizei, *mut GLuint),
    pub glGenRenderbuffers: unsafe extern "C" fn(GLsizei, *mut GLuint),
    pub glGenTextures: unsafe extern "C" fn(GLsizei, *mut GLuint),
    pub glGetError: unsafe extern "C" fn() -> GLenum,
    pub glGetIntegerv: unsafe extern "C" fn(GLenum, *mut GLint),
    pub glGetProgramInfoLog: unsafe extern "C" fn(GLuint, GLsizei, *mut GLsizei, *mut c_char),
    pub glGetProgramiv: unsafe extern "C" fn(GLuint, GLenum, *mut GLint),
    pub glGetShaderInfoLog: unsafe extern "C" fn(GLuint, GLsizei, *mut GLsizei, *mut c_char),
    pub glGetShaderiv: unsafe extern "C" fn(GLuint, GLenum, *mut GLint),
    pub glGetString: unsafe extern "C" fn(GLenum) -> *const u8,
    pub glGetUniformLocation: unsafe extern "C" fn(GLuint, *const c_char) -> GLint,
    pub glLinkProgram: unsafe extern "C" fn(GLuint),
    pub glPixelStorei: unsafe extern "C" fn(GLenum, GLint),
    pub glReadPixels:
        unsafe extern "C" fn(GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, *mut c_void),
    pub glRenderbufferStorage: unsafe extern "C" fn(GLenum, GLenum, GLsizei, GLsizei),
    pub glScissor: unsafe extern "C" fn(GLint, GLint, GLsizei, GLsizei),
    pub glShaderSource:
        unsafe extern "C" fn(GLuint, GLsizei, *const *const c_char, *const GLint),
    pub glTexImage2D: unsafe extern "C" fn(
        GLenum,
        GLint,
        GLint,
        GLsizei,
        GLsizei,
        GLint,
        GLenum,
        GLenum,
        *const c_void,
    ),
    pub glTexParameteri: unsafe extern "C" fn(GLenum, GLenum, GLint),
    pub glTexSubImage2D: unsafe extern "C" fn(
        GLenum,
        GLint,
        GLint,
        GLint,
        GLsizei,
        GLsizei,
        GLenum,
        GLenum,
        *const c_void,
    ),
    pub glUniform1f: unsafe extern "C" fn(GLint, GLfloat),
    pub glUniform1i: unsafe extern "C" fn(GLint, GLint),
    pub glUniform2f: unsafe extern "C" fn(GLint, GLfloat, GLfloat),
    pub glUniform4f: unsafe extern "C" fn(GLint, GLfloat, GLfloat, GLfloat, GLfloat),
    pub glUseProgram: unsafe extern "C" fn(GLuint),
    pub glVertexAttribPointer:
        unsafe extern "C" fn(GLuint, GLint, GLenum, GLboolean, GLsizei, *const c_void),
    pub glViewport: unsafe extern "C" fn(GLint, GLint, GLsizei, GLsizei),
}

unsafe fn load_sym<T: Copy>(lib: &Library, name: &[u8]) -> Option<T> {
    lib.get::<T>(name).ok().map(|s| *s)
}

unsafe fn resolve_gl_sym<T: Copy>(
    get_proc: Option<GlGetProcAddressFn>,
    fallback_lib: &mut Option<Library>,
    name: &[u8],
) -> Option<T> {
    debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*const c_void>());
    debug_assert_eq!(name.last(), Some(&0));
    if let Some(loader) = get_proc {
        let ptr = loader(name.as_ptr() as *const c_char);
        if !ptr.is_null() {
            return Some(std::mem::transmute_copy::<*const c_void, T>(&ptr));
        }
    }
    if fallback_lib.is_none() {
        *fallback_lib = Library::new("libGLESv2.so.2")
            .or_else(|_| Library::new("libGLESv2.so"))
            .ok();
    }
    let lib = fallback_lib.as_ref()?;
    load_sym(lib, name)
}

impl EglProcs {
    unsafe fn load(lib: &Library) -> Option<Self> {
        Some(Self {
            eglGetDisplay: load_sym(lib, b"eglGetDisplay\0")?,
            eglGetPlatformDisplay: load_sym(lib, b"eglGetPlatformDisplay\0"),
            eglInitialize: load_sym(lib, b"eglInitialize\0")?,
            eglTerminate: load_sym(lib, b"eglTerminate\0")?,
            eglBindAPI: load_sym(lib, b"eglBindAPI\0")?,
            eglChooseConfig: load_sym(lib, b"eglChooseConfig\0")?,
            eglCreatePbufferSurface: load_sym(lib, b"eglCreatePbufferSurface\0")?,
            eglDestroySurface: load_sym(lib, b"eglDestroySurface\0")?,
            eglCreateContext: load_sym(lib, b"eglCreateContext\0")?,
            eglDestroyContext: load_sym(lib, b"eglDestroyContext\0")?,
            eglMakeCurrent: load_sym(lib, b"eglMakeCurrent\0")?,
            eglGetCurrentContext: load_sym(lib, b"eglGetCurrentContext\0")?,
            eglGetProcAddress: load_sym(lib, b"eglGetProcAddress\0"),
        })
    }
}

impl Gles2Procs {
    unsafe fn load_with_loader(get_proc: Option<GlGetProcAddressFn>) -> Option<(Self, Option<Library>)> {
        let mut fallback_lib: Option<Library> = None;
        let procs = Self {
            glActiveTexture: resolve_gl_sym(get_proc, &mut fallback_lib, b"glActiveTexture\0")?,
            glAttachShader: resolve_gl_sym(get_proc, &mut fallback_lib, b"glAttachShader\0")?,
            glBindAttribLocation: resolve_gl_sym(get_proc, &mut fallback_lib, b"glBindAttribLocation\0")?,
            glBindBuffer: resolve_gl_sym(get_proc, &mut fallback_lib, b"glBindBuffer\0")?,
            glBindFramebuffer: resolve_gl_sym(get_proc, &mut fallback_lib, b"glBindFramebuffer\0")?,
            glBindRenderbuffer: resolve_gl_sym(get_proc, &mut fallback_lib, b"glBindRenderbuffer\0")?,
            glBindTexture: resolve_gl_sym(get_proc, &mut fallback_lib, b"glBindTexture\0")?,
            glBufferData: resolve_gl_sym(get_proc, &mut fallback_lib, b"glBufferData\0")?,
            glBufferSubData: resolve_gl_sym(get_proc, &mut fallback_lib, b"glBufferSubData\0")?,
            glCheckFramebufferStatus: resolve_gl_sym(get_proc, &mut fallback_lib, b"glCheckFramebufferStatus\0")?,
            glClear: resolve_gl_sym(get_proc, &mut fallback_lib, b"glClear\0")?,
            glClearColor: resolve_gl_sym(get_proc, &mut fallback_lib, b"glClearColor\0")?,
            glClearDepthf: resolve_gl_sym(get_proc, &mut fallback_lib, b"glClearDepthf\0")?,
            glColorMask: resolve_gl_sym(get_proc, &mut fallback_lib, b"glColorMask\0")?,
            glCompileShader: resolve_gl_sym(get_proc, &mut fallback_lib, b"glCompileShader\0")?,
            glCreateProgram: resolve_gl_sym(get_proc, &mut fallback_lib, b"glCreateProgram\0")?,
            glCreateShader: resolve_gl_sym(get_proc, &mut fallback_lib, b"glCreateShader\0")?,
            glDeleteBuffers: resolve_gl_sym(get_proc, &mut fallback_lib, b"glDeleteBuffers\0")?,
            glDeleteFramebuffers: resolve_gl_sym(get_proc, &mut fallback_lib, b"glDeleteFramebuffers\0")?,
            glDeleteProgram: resolve_gl_sym(get_proc, &mut fallback_lib, b"glDeleteProgram\0")?,
            glDeleteRenderbuffers: resolve_gl_sym(get_proc, &mut fallback_lib, b"glDeleteRenderbuffers\0")?,
            glDeleteShader: resolve_gl_sym(get_proc, &mut fallback_lib, b"glDeleteShader\0")?,
            glDeleteTextures: resolve_gl_sym(get_proc, &mut fallback_lib, b"glDeleteTextures\0")?,
            glDepthFunc: resolve_gl_sym(get_proc, &mut fallback_lib, b"glDepthFunc\0")?,
            glDepthMask: resolve_gl_sym(get_proc, &mut fallback_lib, b"glDepthMask\0")?,
            glDisable: resolve_gl_sym(get_proc, &mut fallback_lib, b"glDisable\0")?,
            glDisableVertexAttribArray: resolve_gl_sym(get_proc, &mut fallback_lib, b"glDisableVertexAttribArray\0")?,
            glDrawArrays: resolve_gl_sym(get_proc, &mut fallback_lib, b"glDrawArrays\0")?,
            glDrawElements: resolve_gl_sym(get_proc, &mut fallback_lib, b"glDrawElements\0")?,
            glEnable: resolve_gl_sym(get_proc, &mut fallback_lib, b"glEnable\0")?,
            glEnableVertexAttribArray: resolve_gl_sym(get_proc, &mut fallback_lib, b"glEnableVertexAttribArray\0")?,
            glFinish: resolve_gl_sym(get_proc, &mut fallback_lib, b"glFinish\0")?,
            glFlush: resolve_gl_sym(get_proc, &mut fallback_lib, b"glFlush\0")?,
            glFramebufferRenderbuffer: resolve_gl_sym(get_proc, &mut fallback_lib, b"glFramebufferRenderbuffer\0")?,
            glFramebufferTexture2D: resolve_gl_sym(get_proc, &mut fallback_lib, b"glFramebufferTexture2D\0")?,
            glGenBuffers: resolve_gl_sym(get_proc, &mut fallback_lib, b"glGenBuffers\0")?,
            glGenFramebuffers: resolve_gl_sym(get_proc, &mut fallback_lib, b"glGenFramebuffers\0")?,
            glGenRenderbuffers: resolve_gl_sym(get_proc, &mut fallback_lib, b"glGenRenderbuffers\0")?,
            glGenTextures: resolve_gl_sym(get_proc, &mut fallback_lib, b"glGenTextures\0")?,
            glGetError: resolve_gl_sym(get_proc, &mut fallback_lib, b"glGetError\0")?,
            glGetIntegerv: resolve_gl_sym(get_proc, &mut fallback_lib, b"glGetIntegerv\0")?,
            glGetProgramInfoLog: resolve_gl_sym(get_proc, &mut fallback_lib, b"glGetProgramInfoLog\0")?,
            glGetProgramiv: resolve_gl_sym(get_proc, &mut fallback_lib, b"glGetProgramiv\0")?,
            glGetShaderInfoLog: resolve_gl_sym(get_proc, &mut fallback_lib, b"glGetShaderInfoLog\0")?,
            glGetShaderiv: resolve_gl_sym(get_proc, &mut fallback_lib, b"glGetShaderiv\0")?,
            glGetString: resolve_gl_sym(get_proc, &mut fallback_lib, b"glGetString\0")?,
            glGetUniformLocation: resolve_gl_sym(get_proc, &mut fallback_lib, b"glGetUniformLocation\0")?,
            glLinkProgram: resolve_gl_sym(get_proc, &mut fallback_lib, b"glLinkProgram\0")?,
            glPixelStorei: resolve_gl_sym(get_proc, &mut fallback_lib, b"glPixelStorei\0")?,
            glReadPixels: resolve_gl_sym(get_proc, &mut fallback_lib, b"glReadPixels\0")?,
            glRenderbufferStorage: resolve_gl_sym(get_proc, &mut fallback_lib, b"glRenderbufferStorage\0")?,
            glScissor: resolve_gl_sym(get_proc, &mut fallback_lib, b"glScissor\0")?,
            glShaderSource: resolve_gl_sym(get_proc, &mut fallback_lib, b"glShaderSource\0")?,
            glTexImage2D: resolve_gl_sym(get_proc, &mut fallback_lib, b"glTexImage2D\0")?,
            glTexParameteri: resolve_gl_sym(get_proc, &mut fallback_lib, b"glTexParameteri\0")?,
            glTexSubImage2D: resolve_gl_sym(get_proc, &mut fallback_lib, b"glTexSubImage2D\0")?,
            glUniform1f: resolve_gl_sym(get_proc, &mut fallback_lib, b"glUniform1f\0")?,
            glUniform1i: resolve_gl_sym(get_proc, &mut fallback_lib, b"glUniform1i\0")?,
            glUniform2f: resolve_gl_sym(get_proc, &mut fallback_lib, b"glUniform2f\0")?,
            glUniform4f: resolve_gl_sym(get_proc, &mut fallback_lib, b"glUniform4f\0")?,
            glUseProgram: resolve_gl_sym(get_proc, &mut fallback_lib, b"glUseProgram\0")?,
            glVertexAttribPointer: resolve_gl_sym(get_proc, &mut fallback_lib, b"glVertexAttribPointer\0")?,
            glViewport: resolve_gl_sym(get_proc, &mut fallback_lib, b"glViewport\0")?,
        };
        Some((procs, fallback_lib))
    }
}

struct OwnedEgl {
    _egl_lib: Library,
    egl: EglProcs,
    display: EGLDisplay,
    surface: EGLSurface,
    context: EGLContext,
}

pub struct EglContextGuard {
    owned_egl: Option<OwnedEgl>,
    _gles_lib: Option<Library>,
    pub gl: Gles2Procs,
}

impl EglContextGuard {
    pub fn new_headless() -> Option<Self> {
        unsafe {
            let egl_lib = Library::new("libEGL.so.1").ok()?;
            let egl = EglProcs::load(&egl_lib)?;
            let (gl, gles_lib) = Gles2Procs::load_with_loader(None)?;

            let mut display = ptr::null_mut();
            if let Some(get_plat) = egl.eglGetPlatformDisplay {
                display = get_plat(EGL_PLATFORM_SURFACELESS_MESA, ptr::null_mut(), ptr::null());
            }
            if display.is_null() {
                display = (egl.eglGetDisplay)(ptr::null_mut());
            }
            if display.is_null() {
                return None;
            }

            let mut maj = 0;
            let mut min = 0;
            if (egl.eglInitialize)(display, &mut maj, &mut min) == EGL_FALSE {
                return None;
            }
            if (egl.eglBindAPI)(EGL_OPENGL_ES_API) == EGL_FALSE {
                (egl.eglTerminate)(display);
                return None;
            }

            let cfg_attribs = [
                EGL_SURFACE_TYPE,
                EGL_PBUFFER_BIT,
                EGL_RENDERABLE_TYPE,
                EGL_OPENGL_ES2_BIT,
                EGL_RED_SIZE,
                8,
                EGL_GREEN_SIZE,
                8,
                EGL_BLUE_SIZE,
                8,
                EGL_ALPHA_SIZE,
                8,
                EGL_NONE,
            ];
            let mut config: EGLConfig = ptr::null_mut();
            let mut num_configs: EGLint = 0;
            (egl.eglChooseConfig)(display, cfg_attribs.as_ptr(), &mut config, 1, &mut num_configs);

            let ctx_attribs = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
            let mut context: EGLContext = ptr::null_mut();
            let mut surface: EGLSurface = ptr::null_mut();

            if num_configs > 0 && !config.is_null() {
                context = (egl.eglCreateContext)(display, config, ptr::null_mut(), ctx_attribs.as_ptr());
                if !context.is_null()
                    && (egl.eglMakeCurrent)(display, ptr::null_mut(), ptr::null_mut(), context) == EGL_FALSE
                {
                    let pb_attribs = [EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE];
                    surface = (egl.eglCreatePbufferSurface)(display, config, pb_attribs.as_ptr());
                    if surface.is_null()
                        || (egl.eglMakeCurrent)(display, surface, surface, context) == EGL_FALSE
                    {
                        if !surface.is_null() {
                            (egl.eglDestroySurface)(display, surface);
                        }
                        (egl.eglDestroyContext)(display, context);
                        context = ptr::null_mut();
                        surface = ptr::null_mut();
                    }
                }
            }

            // Fallback: EGL_KHR_no_config_context + surfaceless
            if context.is_null() {
                context = (egl.eglCreateContext)(
                    display,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ctx_attribs.as_ptr(),
                );
                if context.is_null()
                    || (egl.eglMakeCurrent)(display, ptr::null_mut(), ptr::null_mut(), context) == EGL_FALSE
                {
                    if !context.is_null() {
                        (egl.eglDestroyContext)(display, context);
                    }
                    (egl.eglTerminate)(display);
                    return None;
                }
            }

            Some(Self {
                owned_egl: Some(OwnedEgl {
                    _egl_lib: egl_lib,
                    egl,
                    display,
                    surface,
                    context,
                }),
                _gles_lib: gles_lib,
                gl,
            })
        }
    }

    /// Borrows the thread-local current GLES 2.0 context without touching EGL.
    pub unsafe fn from_current_context(get_proc_address: Option<GlGetProcAddressFn>) -> Option<Self> {
        let (gl, gles_lib) = Gles2Procs::load_with_loader(get_proc_address)?;
        Some(Self {
            owned_egl: None,
            _gles_lib: gles_lib,
            gl,
        })
    }

    pub const fn is_external(&self) -> bool {
        self.owned_egl.is_none()
    }

    pub fn make_current(&self) -> bool {
        let Some(owned) = &self.owned_egl else {
            return true;
        };
        unsafe {
            if (owned.egl.eglGetCurrentContext)() == owned.context {
                return true;
            }
            (owned.egl.eglMakeCurrent)(owned.display, owned.surface, owned.surface, owned.context)
                != EGL_FALSE
        }
    }

    #[cfg(test)]
    pub const fn has_fallback_gles_lib(&self) -> bool {
        self._gles_lib.is_some()
    }

    #[cfg(test)]
    pub fn egl_current_context_matches(&self) -> bool {
        self.owned_egl
            .as_ref()
            .is_some_and(|owned| unsafe { (owned.egl.eglGetCurrentContext)() == owned.context })
    }

    #[cfg(test)]
    pub fn egl_get_proc_address(&self) -> Option<GlGetProcAddressFn> {
        self.owned_egl.as_ref().and_then(|owned| owned.egl.eglGetProcAddress)
    }

    pub fn renderer_string(&self) -> String {
        unsafe {
            let p = (self.gl.glGetString)(0x1F01);
            if p.is_null() {
                String::new()
            } else {
                CStr::from_ptr(p as *const c_char).to_string_lossy().into_owned()
            }
        }
    }

    pub fn max_texture_size(&self) -> u32 {
        unsafe {
            let mut v = 2048;
            (self.gl.glGetIntegerv)(GL_MAX_TEXTURE_SIZE, &mut v);
            v.max(2048) as u32
        }
    }

    pub fn max_texture_units(&self) -> u32 {
        unsafe {
            let mut v: GLint = 0;
            (self.gl.glGetIntegerv)(GL_MAX_TEXTURE_IMAGE_UNITS, &mut v);
            v.max(0) as u32
        }
    }
}

impl Drop for EglContextGuard {
    fn drop(&mut self) {
        let Some(owned) = &self.owned_egl else {
            return;
        };
        unsafe {
            (owned.egl.eglMakeCurrent)(
                owned.display,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            if !owned.surface.is_null() {
                (owned.egl.eglDestroySurface)(owned.display, owned.surface);
            }
            if !owned.context.is_null() {
                (owned.egl.eglDestroyContext)(owned.display, owned.context);
            }
            if !owned.display.is_null() {
                (owned.egl.eglTerminate)(owned.display);
            }
        }
    }
}

pub fn compile_program(
    gl: &Gles2Procs,
    vs_src: &str,
    fs_src: &str,
    attribs: &[(GLuint, &str)],
) -> Result<GLuint, String> {
    unsafe {
        let vs = compile_shader(gl, GL_VERTEX_SHADER, vs_src)?;
        let fs = match compile_shader(gl, GL_FRAGMENT_SHADER, fs_src) {
            Ok(s) => s,
            Err(e) => {
                (gl.glDeleteShader)(vs);
                return Err(e);
            }
        };
        let prog = (gl.glCreateProgram)();
        if prog == 0 {
            (gl.glDeleteShader)(vs);
            (gl.glDeleteShader)(fs);
            return Err("GL program creation failed".to_string());
        }
        (gl.glAttachShader)(prog, vs);
        (gl.glAttachShader)(prog, fs);
        for &(idx, name) in attribs {
            let cname = CString::new(name).unwrap();
            (gl.glBindAttribLocation)(prog, idx, cname.as_ptr());
        }
        (gl.glLinkProgram)(prog);
        (gl.glDeleteShader)(vs);
        (gl.glDeleteShader)(fs);
        let mut status = 0;
        (gl.glGetProgramiv)(prog, GL_LINK_STATUS, &mut status);
        if status == 0 {
            let mut buf = vec![0u8; 1024];
            let mut len = 0;
            (gl.glGetProgramInfoLog)(prog, buf.len() as GLsizei, &mut len, buf.as_mut_ptr() as *mut c_char);
            (gl.glDeleteProgram)(prog);
            buf.truncate(len.max(0) as usize);
            return Err(format!("GL program link failed: {}", String::from_utf8_lossy(&buf)));
        }
        Ok(prog)
    }
}

unsafe fn compile_shader(gl: &Gles2Procs, ty: GLenum, src: &str) -> Result<GLuint, String> {
    let shader = (gl.glCreateShader)(ty);
    if shader == 0 {
        return Err("GL shader creation failed".to_string());
    }
    let ptr = src.as_ptr() as *const c_char;
    let len = src.len() as GLint;
    (gl.glShaderSource)(shader, 1, &ptr, &len);
    (gl.glCompileShader)(shader);
    let mut status = 0;
    (gl.glGetShaderiv)(shader, GL_COMPILE_STATUS, &mut status);
    if status == 0 {
        let mut buf = vec![0u8; 1024];
        let mut out_len = 0;
        (gl.glGetShaderInfoLog)(shader, buf.len() as GLsizei, &mut out_len, buf.as_mut_ptr() as *mut c_char);
        (gl.glDeleteShader)(shader);
        buf.truncate(out_len.max(0) as usize);
        return Err(format!("GL shader compile failed: {}", String::from_utf8_lossy(&buf)));
    }
    Ok(shader)
}

pub fn uniform_loc(gl: &Gles2Procs, prog: GLuint, name: &str) -> GLint {
    let cname = CString::new(name).unwrap();
    unsafe { (gl.glGetUniformLocation)(prog, cname.as_ptr()) }
}
