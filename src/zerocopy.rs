//! Zero-copy page frames (Linux): Servo's render target is exported as a dma-buf and imported as a
//! texture into Slint's GL context, so a frame never passes through CPU memory.
use std::ffi::{c_char, c_void, CStr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

type Ptr = *mut c_void;

const EGL_NONE: i32 = 0x3038;
const EGL_EXTENSIONS: i32 = 0x3055;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_WIDTH: i32 = 0x3057;
const EGL_GL_TEXTURE_2D: u32 = 0x30B1;
const EGL_LINUX_DMA_BUF: u32 = 0x3270;
const EGL_LINUX_DRM_FOURCC: i32 = 0x3271;
const EGL_PLANE0_FD: i32 = 0x3272;
const EGL_PLANE0_OFFSET: i32 = 0x3273;
const EGL_PLANE0_PITCH: i32 = 0x3274;
const EGL_PLANE0_MODIFIER_LO: i32 = 0x3443;
const EGL_PLANE0_MODIFIER_HI: i32 = 0x3444;
const MODIFIER_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_BINDING_2D: u32 = 0x8069;
const GL_RENDERER: u32 = 0x1F01;

/// One exported render target. Lives as long as the Servo surface it came from.
pub struct DmabufFrame {
    pub id: u64,
    pub fd: OwnedFd,
    pub fourcc: i32,
    pub modifier: u64,
    pub stride: i32,
    pub offset: i32,
    pub width: u32,
    pub height: u32,
}

struct Egl {
    _lib: libloading::Library,
    get_current_display: unsafe extern "C" fn() -> Ptr,
    get_current_context: unsafe extern "C" fn() -> Ptr,
    get_current_surface: unsafe extern "C" fn(i32) -> Ptr,
    query_api: unsafe extern "C" fn() -> u32,
    bind_api: unsafe extern "C" fn(u32) -> u32,
    make_current: unsafe extern "C" fn(Ptr, Ptr, Ptr, Ptr) -> u32,
    query_string: unsafe extern "C" fn(Ptr, i32) -> *const c_char,
    create_image: unsafe extern "C" fn(Ptr, Ptr, u32, Ptr, *const i32) -> Ptr,
    destroy_image: unsafe extern "C" fn(Ptr, Ptr) -> u32,
    export_query: Option<unsafe extern "C" fn(Ptr, Ptr, *mut i32, *mut i32, *mut u64) -> u32>,
    export: Option<unsafe extern "C" fn(Ptr, Ptr, *mut i32, *mut i32, *mut i32) -> u32>,
}

fn egl() -> Option<&'static Egl> {
    static EGL: std::sync::OnceLock<Option<Egl>> = std::sync::OnceLock::new();
    EGL.get_or_init(|| unsafe {
        let lib = libloading::Library::new("libEGL.so.1").ok()?;
        let gpa: unsafe extern "C" fn(*const c_char) -> Ptr = *lib.get(b"eglGetProcAddress\0").ok()?;
        let ext = |name: &CStr| {
            let p = gpa(name.as_ptr());
            (!p.is_null()).then_some(p)
        };
        Some(Egl {
            get_current_display: *lib.get(b"eglGetCurrentDisplay\0").ok()?,
            get_current_context: *lib.get(b"eglGetCurrentContext\0").ok()?,
            get_current_surface: *lib.get(b"eglGetCurrentSurface\0").ok()?,
            query_api: *lib.get(b"eglQueryAPI\0").ok()?,
            bind_api: *lib.get(b"eglBindAPI\0").ok()?,
            make_current: *lib.get(b"eglMakeCurrent\0").ok()?,
            query_string: *lib.get(b"eglQueryString\0").ok()?,
            create_image: std::mem::transmute::<Ptr, _>(ext(c"eglCreateImageKHR")?),
            destroy_image: std::mem::transmute::<Ptr, _>(ext(c"eglDestroyImageKHR")?),
            export_query: ext(c"eglExportDMABUFImageQueryMESA").map(|p| std::mem::transmute::<Ptr, _>(p)),
            export: ext(c"eglExportDMABUFImageMESA").map(|p| std::mem::transmute::<Ptr, _>(p)),
            _lib: lib,
        })
    })
    .as_ref()
}

fn display_has(egl: &Egl, dpy: Ptr, ext: &str) -> bool {
    let s = unsafe { (egl.query_string)(dpy, EGL_EXTENSIONS) };
    !s.is_null() && unsafe { CStr::from_ptr(s) }.to_string_lossy().split(' ').any(|e| e == ext)
}

/// Remembers which GL context is current on this thread and puts it back when dropped.
/// The UI toolkit issues GL calls outside its render pass and assumes its context stays current,
/// so every call into Servo (which switches to its own context) is wrapped in one of these.
pub struct CurrentGuard(Option<(u32, Ptr, Ptr, Ptr, Ptr)>);

impl CurrentGuard {
    pub fn save() -> Self {
        Self(egl().and_then(|egl| unsafe {
            let ctx = (egl.get_current_context)();
            (!ctx.is_null()).then(|| ((egl.query_api)(), (egl.get_current_display)(), (egl.get_current_surface)(0x3059), (egl.get_current_surface)(0x305A), ctx))
        }))
    }
}

impl Drop for CurrentGuard {
    fn drop(&mut self) {
        if let (Some((api, dpy, draw, read, ctx)), Some(egl)) = (self.0, egl()) {
            unsafe {
                (egl.bind_api)(api);
                (egl.make_current)(dpy, draw, read, ctx);
            }
        }
    }
}

/// Export GL texture `texture` of the context that is current on this thread (Servo's).
pub fn export_texture(texture: u32, width: u32, height: u32) -> Result<DmabufFrame, String> {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let egl = egl().ok_or("libEGL not available")?;
    unsafe {
        let (dpy, ctx) = ((egl.get_current_display)(), (egl.get_current_context)());
        if dpy.is_null() || ctx.is_null() {
            return Err("no current EGL context".into());
        }
        let (Some(query), Some(export)) = (egl.export_query, egl.export) else { return Err("no dma-buf export functions".into()) };
        if !display_has(egl, dpy, "EGL_MESA_image_dma_buf_export") {
            return Err("EGL_MESA_image_dma_buf_export missing".into());
        }
        let image = (egl.create_image)(dpy, ctx, EGL_GL_TEXTURE_2D, texture as usize as Ptr, [EGL_NONE].as_ptr());
        if image.is_null() {
            return Err("eglCreateImage(texture) failed".into());
        }
        let (mut fourcc, mut planes, mut modifier) = (0i32, 0i32, [0u64; 4]);
        let (mut fd, mut stride, mut offset) = ([-1i32; 4], [0i32; 4], [0i32; 4]);
        let ok = query(dpy, image, &mut fourcc, &mut planes, modifier.as_mut_ptr()) != 0
            && planes == 1
            && export(dpy, image, fd.as_mut_ptr(), stride.as_mut_ptr(), offset.as_mut_ptr()) != 0
            && fd[0] >= 0;
        (egl.destroy_image)(dpy, image);
        if !ok {
            return Err(format!("dma-buf export failed (planes={planes})"));
        }
        Ok(DmabufFrame {
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            fd: OwnedFd::from_raw_fd(fd[0]),
            fourcc,
            modifier: modifier[0],
            stride: stride[0],
            offset: offset[0],
            width,
            height,
        })
    }
}

/// GL functions of the UI (Slint) context, plus the texture currently imported into it.
pub struct Importer {
    gen_textures: unsafe extern "C" fn(i32, *mut u32),
    delete_textures: unsafe extern "C" fn(i32, *const u32),
    bind_texture: unsafe extern "C" fn(u32, u32),
    tex_parameteri: unsafe extern "C" fn(u32, u32, i32),
    get_integerv: unsafe extern "C" fn(u32, *mut i32),
    image_target: unsafe extern "C" fn(u32, Ptr),
    get_error: unsafe extern "C" fn() -> u32,
    pub renderer: String,
    current: Option<(u32, Ptr, Ptr)>,
    pub id: u64,
}

impl Importer {
    /// Must be called with the UI context current. `get_proc` resolves GL symbols in it.
    pub fn new(get_proc: &dyn Fn(&CStr) -> *const c_void) -> Option<Self> {
        let f = |n: &CStr| {
            let p = get_proc(n);
            (!p.is_null()).then_some(p)
        };
        unsafe {
            let get_string: unsafe extern "C" fn(u32) -> *const c_char = std::mem::transmute(f(c"glGetString")?);
            let r = get_string(GL_RENDERER);
            Some(Self {
                gen_textures: std::mem::transmute::<*const c_void, _>(f(c"glGenTextures")?),
                delete_textures: std::mem::transmute::<*const c_void, _>(f(c"glDeleteTextures")?),
                bind_texture: std::mem::transmute::<*const c_void, _>(f(c"glBindTexture")?),
                tex_parameteri: std::mem::transmute::<*const c_void, _>(f(c"glTexParameteri")?),
                get_integerv: std::mem::transmute::<*const c_void, _>(f(c"glGetIntegerv")?),
                image_target: std::mem::transmute::<*const c_void, _>(f(c"glEGLImageTargetTexture2DOES")?),
                get_error: std::mem::transmute::<*const c_void, _>(f(c"glGetError")?),
                renderer: if r.is_null() { String::new() } else { CStr::from_ptr(r).to_string_lossy().into_owned() },
                current: None,
                id: 0,
            })
        }
    }

    pub fn is_software(&self) -> bool {
        let r = self.renderer.to_lowercase();
        ["llvmpipe", "softpipe", "swrast", "software"].iter().any(|s| r.contains(s))
    }

    /// Import `frame` as a GL texture in the current (UI) context; returns the texture name.
    /// The previously imported texture stays valid until `release_old` is called with it.
    pub fn import(&mut self, frame: &DmabufFrame) -> Result<(u32, Option<(u32, Ptr, Ptr)>), String> {
        let egl = egl().ok_or("libEGL not available")?;
        unsafe {
            let dpy = (egl.get_current_display)();
            if dpy.is_null() {
                return Err("UI context is not EGL".into());
            }
            // A page is opaque: import as the X (no alpha) variant so stray alpha can't blend with the UI behind it.
            let fourcc = match frame.fourcc {
                0x3432_4241 => 0x3432_4258, // AB24 -> XB24
                0x3432_5241 => 0x3432_5258, // AR24 -> XR24
                f => f,
            };
            let mut attrs = vec![
                EGL_WIDTH, frame.width as i32, EGL_HEIGHT, frame.height as i32, EGL_LINUX_DRM_FOURCC, fourcc,
                EGL_PLANE0_FD, frame.fd.as_raw_fd(), EGL_PLANE0_OFFSET, frame.offset, EGL_PLANE0_PITCH, frame.stride,
            ];
            if frame.modifier != MODIFIER_INVALID && display_has(egl, dpy, "EGL_EXT_image_dma_buf_import_modifiers") {
                attrs.extend([EGL_PLANE0_MODIFIER_LO, frame.modifier as u32 as i32, EGL_PLANE0_MODIFIER_HI, (frame.modifier >> 32) as u32 as i32]);
            }
            attrs.push(EGL_NONE);
            let image = (egl.create_image)(dpy, std::ptr::null_mut(), EGL_LINUX_DMA_BUF, std::ptr::null_mut(), attrs.as_ptr());
            if image.is_null() {
                return Err(format!("dma-buf import failed (fourcc={:#x} modifier={:#x})", frame.fourcc, frame.modifier));
            }
            let (mut prev, mut tex) = (0i32, 0u32);
            (self.get_integerv)(GL_TEXTURE_BINDING_2D, &mut prev);
            (self.gen_textures)(1, &mut tex);
            (self.bind_texture)(GL_TEXTURE_2D, tex);
            for (k, v) in [(0x2801, 0x2601), (0x2800, 0x2601), (0x2802, 0x812F), (0x2803, 0x812F)] {
                (self.tex_parameteri)(GL_TEXTURE_2D, k, v);
            }
            while (self.get_error)() != 0 {}
            (self.image_target)(GL_TEXTURE_2D, image);
            let err = (self.get_error)();
            if err != 0 {
                crate::log_line(&format!("zero-copy: binding the image to a texture gave GL error {err:#x}"));
            }
            (self.bind_texture)(GL_TEXTURE_2D, prev as u32);
            let old = self.current.replace((tex, image, dpy));
            self.id = frame.id;
            Ok((tex, old))
        }
    }

    pub fn release(&self, old: Option<(u32, Ptr, Ptr)>) {
        if let (Some((tex, image, dpy)), Some(egl)) = (old, egl()) {
            unsafe {
                (self.delete_textures)(1, &tex);
                (egl.destroy_image)(dpy, image);
            }
        }
    }

    pub fn teardown(&mut self) {
        let old = self.current.take();
        self.release(old);
        self.id = 0;
    }
}

/// True when the Wayland compositor lets clients share GPU buffers (so the UI can render on the GPU).
pub fn wayland_has_dmabuf() -> bool {
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::wl_registry;
    struct S;
    impl wayland_client::Dispatch<wl_registry::WlRegistry, GlobalListContents> for S {
        fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &wayland_client::Connection, _: &wayland_client::QueueHandle<Self>) {}
    }
    let Ok(conn) = wayland_client::Connection::connect_to_env() else { return false };
    let Ok((globals, _queue)) = registry_queue_init::<S>(&conn) else { return false };
    globals.contents().with_list(|l| l.iter().any(|g| g.interface == "zwp_linux_dmabuf_v1"))
}
