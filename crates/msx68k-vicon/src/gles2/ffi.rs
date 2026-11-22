//! Dynamic FFI bindings to `libEGL.so.1` and `libGLESv2.so.2` via `libloading`.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::ptr;

use libloading::Library;

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

#[allow(non_snake_case)]
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
        })
    }
}

impl Gles2Procs {
    unsafe fn load(lib: &Library) -> Option<Self> {
        Some(Self {
            glActiveTexture: load_sym(lib, b"glActiveTexture\0")?,
            glAttachShader: load_sym(lib, b"glAttachShader\0")?,
            glBindAttribLocation: load_sym(lib, b"glBindAttribLocation\0")?,
            glBindBuffer: load_sym(lib, b"glBindBuffer\0")?,
            glBindFramebuffer: load_sym(lib, b"glBindFramebuffer\0")?,
            glBindRenderbuffer: load_sym(lib, b"glBindRenderbuffer\0")?,
            glBindTexture: load_sym(lib, b"glBindTexture\0")?,
            glBufferData: load_sym(lib, b"glBufferData\0")?,
            glBufferSubData: load_sym(lib, b"glBufferSubData\0")?,
            glCheckFramebufferStatus: load_sym(lib, b"glCheckFramebufferStatus\0")?,
            glClear: load_sym(lib, b"glClear\0")?,
            glClearColor: load_sym(lib, b"glClearColor\0")?,
            glClearDepthf: load_sym(lib, b"glClearDepthf\0")?,
            glCompileShader: load_sym(lib, b"glCompileShader\0")?,
            glCreateProgram: load_sym(lib, b"glCreateProgram\0")?,
            glCreateShader: load_sym(lib, b"glCreateShader\0")?,
            glDeleteBuffers: load_sym(lib, b"glDeleteBuffers\0")?,
            glDeleteFramebuffers: load_sym(lib, b"glDeleteFramebuffers\0")?,
            glDeleteProgram: load_sym(lib, b"glDeleteProgram\0")?,
            glDeleteRenderbuffers: load_sym(lib, b"glDeleteRenderbuffers\0")?,
            glDeleteShader: load_sym(lib, b"glDeleteShader\0")?,
            glDeleteTextures: load_sym(lib, b"glDeleteTextures\0")?,
            glDepthFunc: load_sym(lib, b"glDepthFunc\0")?,
            glDepthMask: load_sym(lib, b"glDepthMask\0")?,
            glDisable: load_sym(lib, b"glDisable\0")?,
            glDisableVertexAttribArray: load_sym(lib, b"glDisableVertexAttribArray\0")?,
            glDrawArrays: load_sym(lib, b"glDrawArrays\0")?,
            glDrawElements: load_sym(lib, b"glDrawElements\0")?,
            glEnable: load_sym(lib, b"glEnable\0")?,
            glEnableVertexAttribArray: load_sym(lib, b"glEnableVertexAttribArray\0")?,
            glFinish: load_sym(lib, b"glFinish\0")?,
            glFlush: load_sym(lib, b"glFlush\0")?,
            glFramebufferRenderbuffer: load_sym(lib, b"glFramebufferRenderbuffer\0")?,
            glFramebufferTexture2D: load_sym(lib, b"glFramebufferTexture2D\0")?,
            glGenBuffers: load_sym(lib, b"glGenBuffers\0")?,
            glGenFramebuffers: load_sym(lib, b"glGenFramebuffers\0")?,
            glGenRenderbuffers: load_sym(lib, b"glGenRenderbuffers\0")?,
            glGenTextures: load_sym(lib, b"glGenTextures\0")?,
            glGetError: load_sym(lib, b"glGetError\0")?,
            glGetIntegerv: load_sym(lib, b"glGetIntegerv\0")?,
            glGetProgramInfoLog: load_sym(lib, b"glGetProgramInfoLog\0")?,
            glGetProgramiv: load_sym(lib, b"glGetProgramiv\0")?,
            glGetShaderInfoLog: load_sym(lib, b"glGetShaderInfoLog\0")?,
            glGetShaderiv: load_sym(lib, b"glGetShaderiv\0")?,
            glGetString: load_sym(lib, b"glGetString\0")?,
            glGetUniformLocation: load_sym(lib, b"glGetUniformLocation\0")?,
            glLinkProgram: load_sym(lib, b"glLinkProgram\0")?,
            glPixelStorei: load_sym(lib, b"glPixelStorei\0")?,
            glReadPixels: load_sym(lib, b"glReadPixels\0")?,
            glRenderbufferStorage: load_sym(lib, b"glRenderbufferStorage\0")?,
            glScissor: load_sym(lib, b"glScissor\0")?,
            glShaderSource: load_sym(lib, b"glShaderSource\0")?,
            glTexImage2D: load_sym(lib, b"glTexImage2D\0")?,
            glTexParameteri: load_sym(lib, b"glTexParameteri\0")?,
            glTexSubImage2D: load_sym(lib, b"glTexSubImage2D\0")?,
            glUniform1f: load_sym(lib, b"glUniform1f\0")?,
            glUniform1i: load_sym(lib, b"glUniform1i\0")?,
            glUniform2f: load_sym(lib, b"glUniform2f\0")?,
            glUniform4f: load_sym(lib, b"glUniform4f\0")?,
            glUseProgram: load_sym(lib, b"glUseProgram\0")?,
            glVertexAttribPointer: load_sym(lib, b"glVertexAttribPointer\0")?,
            glViewport: load_sym(lib, b"glViewport\0")?,
        })
    }
}

pub struct EglContextGuard {
    _egl_lib: Library,
    _gles_lib: Library,
    pub egl: EglProcs,
    pub gl: Gles2Procs,
    display: EGLDisplay,
    surface: EGLSurface,
    context: EGLContext,
}

impl EglContextGuard {
    pub fn new_headless() -> Option<Self> {
        unsafe {
            let egl_lib = Library::new("libEGL.so.1").ok()?;
            let gles_lib = Library::new("libGLESv2.so.2").ok()?;
            let egl = EglProcs::load(&egl_lib)?;
            let gl = Gles2Procs::load(&gles_lib)?;

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
                _egl_lib: egl_lib,
                _gles_lib: gles_lib,
                egl,
                gl,
                display,
                surface,
                context,
            })
        }
    }

    pub fn make_current(&self) -> bool {
        unsafe {
            if (self.egl.eglGetCurrentContext)() == self.context {
                return true;
            }
            (self.egl.eglMakeCurrent)(self.display, self.surface, self.surface, self.context) != EGL_FALSE
        }
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
            let mut v = 8;
            (self.gl.glGetIntegerv)(GL_MAX_TEXTURE_IMAGE_UNITS, &mut v);
            v.max(8) as u32
        }
    }
}

impl Drop for EglContextGuard {
    fn drop(&mut self) {
        unsafe {
            (self.egl.eglMakeCurrent)(self.display, ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
            if !self.surface.is_null() {
                (self.egl.eglDestroySurface)(self.display, self.surface);
            }
            if !self.context.is_null() {
                (self.egl.eglDestroyContext)(self.display, self.context);
            }
            if !self.display.is_null() {
                (self.egl.eglTerminate)(self.display);
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
