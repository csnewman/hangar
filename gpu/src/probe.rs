//! What this host renders with: the OpenGL renderer EGL gives on the
//! surfaceless platform, as virglrenderer uses it, and the Vulkan devices
//! Venus's render server would choose among. The libraries are loaded at run
//! time, so a host without one of them says so rather than failing to start.
//!
//! It runs with the environment the backend is given, so what it reports is
//! what environments get: the worker's choice of renderer and device, made
//! through Mesa's environment variables, shows here as it takes effect.

use std::ffi::{c_char, c_void, CStr, CString};

use serde::Serialize;

#[derive(Serialize, Default)]
pub struct Report {
    /// OpenGL, or why there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gl: Option<Gl>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub gl_error: String,
    /// Vulkan's devices, in the order they are listed.
    pub vulkan: Vec<VkDevice>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub vulkan_error: String,
}

#[derive(Serialize)]
pub struct Gl {
    pub vendor: String,
    pub renderer: String,
    pub version: String,
    /// Whether it renders on the CPU: Mesa's llvmpipe or softpipe.
    pub software: bool,
}

#[derive(Serialize)]
pub struct VkDevice {
    pub name: String,
    /// Whether it is a CPU device, such as lavapipe.
    pub software: bool,
}

pub fn run() -> Report {
    let mut r = Report::default();
    match gl() {
        Ok(g) => r.gl = Some(g),
        Err(e) => r.gl_error = e,
    }
    match vulkan() {
        Ok(v) => r.vulkan = v,
        Err(e) => r.vulkan_error = e,
    }
    r
}

/// A shared library, closed when dropped.
struct Library(*mut c_void);

impl Library {
    fn open(names: &[&str]) -> Result<Self, String> {
        for name in names {
            let n = CString::new(*name).unwrap();
            // SAFETY: n is a valid C string; dlopen returns null on failure.
            let h = unsafe { libc::dlopen(n.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if !h.is_null() {
                return Ok(Library(h));
            }
        }
        Err(format!("{} is not installed", names[0]))
    }

    /// The address of a symbol, or an error naming it.
    fn sym(&self, name: &str) -> Result<*mut c_void, String> {
        let n = CString::new(name).unwrap();
        // SAFETY: self.0 is an open handle and n a valid C string.
        let p = unsafe { libc::dlsym(self.0, n.as_ptr()) };
        if p.is_null() {
            return Err(format!("{name} is missing"));
        }
        Ok(p)
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: self.0 was returned by dlopen and is closed once.
        unsafe { libc::dlclose(self.0) };
    }
}

const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_OPENGL_API: u32 = 0x30A2;
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_NONE: i32 = 0x3038;
const GL_VENDOR: u32 = 0x1F00;
const GL_RENDERER: u32 = 0x1F01;
const GL_VERSION: u32 = 0x1F02;

type EglGetPlatformDisplay = unsafe extern "C" fn(u32, *mut c_void, *const isize) -> *mut c_void;
type EglInitialize = unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32;
type EglBindApi = unsafe extern "C" fn(u32) -> u32;
type EglCreateContext =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void;
type EglMakeCurrent =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32;
type EglGetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type EglDestroyContext = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;
type EglTerminate = unsafe extern "C" fn(*mut c_void) -> u32;
type GlGetString = unsafe extern "C" fn(u32) -> *const c_char;

/// The OpenGL renderer a surfaceless EGL context gets: desktop OpenGL, as
/// virglrenderer asks for, or OpenGL ES where there is no desktop OpenGL.
fn gl() -> Result<Gl, String> {
    let egl = Library::open(&["libEGL.so.1", "libEGL.so"])?;
    // SAFETY: each symbol is cast to its EGL signature, and every call below
    // passes arguments of the types those signatures name.
    unsafe {
        let get_display: EglGetPlatformDisplay =
            std::mem::transmute(egl.sym("eglGetPlatformDisplay")?);
        let initialize: EglInitialize = std::mem::transmute(egl.sym("eglInitialize")?);
        let bind_api: EglBindApi = std::mem::transmute(egl.sym("eglBindAPI")?);
        let create_context: EglCreateContext = std::mem::transmute(egl.sym("eglCreateContext")?);
        let make_current: EglMakeCurrent = std::mem::transmute(egl.sym("eglMakeCurrent")?);
        let get_proc: EglGetProcAddress = std::mem::transmute(egl.sym("eglGetProcAddress")?);
        let destroy_context: EglDestroyContext = std::mem::transmute(egl.sym("eglDestroyContext")?);
        let terminate: EglTerminate = std::mem::transmute(egl.sym("eglTerminate")?);

        let dpy = get_display(
            EGL_PLATFORM_SURFACELESS_MESA,
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if dpy.is_null() {
            return Err("EGL has no surfaceless display".into());
        }
        let (mut major, mut minor) = (0, 0);
        if initialize(dpy, &mut major, &mut minor) == 0 {
            return Err("EGL did not initialise its surfaceless display".into());
        }
        let mut ctx = std::ptr::null_mut();
        if bind_api(EGL_OPENGL_API) != 0 {
            ctx = create_context(
                dpy,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                [EGL_NONE].as_ptr(),
            );
        }
        if ctx.is_null() && bind_api(EGL_OPENGL_ES_API) != 0 {
            let attrs = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
            ctx = create_context(
                dpy,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                attrs.as_ptr(),
            );
        }
        if ctx.is_null() {
            terminate(dpy);
            return Err("EGL made no OpenGL context".into());
        }
        let result = (|| {
            if make_current(dpy, std::ptr::null_mut(), std::ptr::null_mut(), ctx) == 0 {
                return Err("EGL could not make a context current without a surface".to_string());
            }
            let name = CString::new("glGetString").unwrap();
            let p = get_proc(name.as_ptr());
            if p.is_null() {
                return Err("glGetString is missing".to_string());
            }
            let get_string: GlGetString = std::mem::transmute(p);
            let s = |what| {
                let v = get_string(what);
                if v.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(v).to_string_lossy().into_owned()
                }
            };
            let renderer = s(GL_RENDERER);
            let lower = renderer.to_lowercase();
            Ok(Gl {
                vendor: s(GL_VENDOR),
                software: lower.contains("llvmpipe")
                    || lower.contains("softpipe")
                    || lower.contains("swrast"),
                renderer,
                version: s(GL_VERSION),
            })
        })();
        make_current(
            dpy,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        destroy_context(dpy, ctx);
        terminate(dpy);
        result
    }
}

const VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO: u32 = 1;
const VK_PHYSICAL_DEVICE_TYPE_CPU: u32 = 4;

#[repr(C)]
struct VkInstanceCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    p_application_info: *const c_void,
    enabled_layer_count: u32,
    pp_enabled_layer_names: *const *const c_char,
    enabled_extension_count: u32,
    pp_enabled_extension_names: *const *const c_char,
}

type VkGetInstanceProcAddr = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type VkCreateInstance =
    unsafe extern "C" fn(*const VkInstanceCreateInfo, *const c_void, *mut *mut c_void) -> i32;
type VkEnumeratePhysicalDevices =
    unsafe extern "C" fn(*mut c_void, *mut u32, *mut *mut c_void) -> i32;
type VkGetPhysicalDeviceProperties = unsafe extern "C" fn(*mut c_void, *mut u8);
type VkDestroyInstance = unsafe extern "C" fn(*mut c_void, *const c_void);

/// VkPhysicalDeviceProperties begins with apiVersion, driverVersion,
/// vendorID, deviceID and deviceType, four bytes each, then deviceName; the
/// whole structure is well under this size, whatever its limits hold.
const PROPERTIES_SIZE: usize = 4096;
const DEVICE_TYPE_AT: usize = 16;
const DEVICE_NAME_AT: usize = 20;
const DEVICE_NAME_LEN: usize = 256;

fn vulkan() -> Result<Vec<VkDevice>, String> {
    let vk = Library::open(&["libvulkan.so.1", "libvulkan.so"])?;
    // SAFETY: each function is cast to its Vulkan signature and called with
    // arguments of those types; the properties buffer is larger than the
    // structure the call fills in.
    unsafe {
        let get_proc: VkGetInstanceProcAddr = std::mem::transmute(vk.sym("vkGetInstanceProcAddr")?);
        let lookup = |instance: *mut c_void, name: &str| -> Result<*mut c_void, String> {
            let n = CString::new(name).unwrap();
            let p = get_proc(instance, n.as_ptr());
            if p.is_null() {
                Err(format!("{name} is missing"))
            } else {
                Ok(p)
            }
        };
        let create: VkCreateInstance =
            std::mem::transmute(lookup(std::ptr::null_mut(), "vkCreateInstance")?);
        let info = VkInstanceCreateInfo {
            s_type: VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            p_application_info: std::ptr::null(),
            enabled_layer_count: 0,
            pp_enabled_layer_names: std::ptr::null(),
            enabled_extension_count: 0,
            pp_enabled_extension_names: std::ptr::null(),
        };
        let mut instance = std::ptr::null_mut();
        if create(&info, std::ptr::null(), &mut instance) != 0 {
            return Err("Vulkan made no instance".into());
        }
        let destroy: VkDestroyInstance =
            std::mem::transmute(lookup(instance, "vkDestroyInstance")?);
        let result = (|| {
            let enumerate: VkEnumeratePhysicalDevices =
                std::mem::transmute(lookup(instance, "vkEnumeratePhysicalDevices")?);
            let properties: VkGetPhysicalDeviceProperties =
                std::mem::transmute(lookup(instance, "vkGetPhysicalDeviceProperties")?);
            let mut count = 0u32;
            if enumerate(instance, &mut count, std::ptr::null_mut()) != 0 {
                return Err("Vulkan did not list its devices".to_string());
            }
            let mut devices = vec![std::ptr::null_mut(); count as usize];
            if enumerate(instance, &mut count, devices.as_mut_ptr()) != 0 {
                return Err("Vulkan did not list its devices".to_string());
            }
            let mut out = Vec::new();
            for d in devices.into_iter().take(count as usize) {
                let mut buf = vec![0u8; PROPERTIES_SIZE];
                properties(d, buf.as_mut_ptr());
                let kind =
                    u32::from_ne_bytes(buf[DEVICE_TYPE_AT..DEVICE_TYPE_AT + 4].try_into().unwrap());
                let name = &buf[DEVICE_NAME_AT..DEVICE_NAME_AT + DEVICE_NAME_LEN];
                let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                out.push(VkDevice {
                    name: String::from_utf8_lossy(&name[..end]).into_owned(),
                    software: kind == VK_PHYSICAL_DEVICE_TYPE_CPU,
                });
            }
            Ok(out)
        })();
        destroy(instance, std::ptr::null());
        result
    }
}
