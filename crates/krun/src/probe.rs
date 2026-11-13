//! What this host renders with, as libkrun's virglrenderer renders on it:
//! the OpenGL renderer EGL gives on the surfaceless platform, and the
//! Vulkan devices Venus's render server would choose among. It runs with
//! the environment the worker gives a machine, so the worker's choice of
//! renderer and device, made through Mesa's environment variables, shows
//! here as it takes effect.

use std::ffi::{CStr, c_char, c_void};

use hangar_api::monitor::{GpuProbe, ProbedGl, ProbedVulkan};

pub fn probe() -> GpuProbe {
    let mut p = GpuProbe::default();
    match gl() {
        Ok((vendor, renderer, version)) => {
            let lower = renderer.to_lowercase();
            p.gl = Some(ProbedGl {
                software: ["llvmpipe", "softpipe", "swrast"]
                    .iter()
                    .any(|s| lower.contains(s)),
                vendor,
                renderer,
                version,
            });
        }
        Err(e) => p.gl_error = e.into(),
    }
    match vulkan() {
        Ok(devices) => p.vulkan = Some(devices),
        Err(e) => p.vulkan_error = e.into(),
    }
    p
}

const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_OPENGL_API: u32 = 0x30A2;
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_NONE: i32 = 0x3038;
const GL_VENDOR: u32 = 0x1F00;
const GL_RENDERER: u32 = 0x1F01;
const GL_VERSION: u32 = 0x1F02;

/// dlsym's answer for name, as a function of type F, or none.
///
/// # Safety
///
/// F must be the function's type.
unsafe fn sym<F: Copy>(lib: *mut c_void, name: &CStr) -> Option<F> {
    // SAFETY: lib is an open library; the caller says what name is.
    let p = unsafe { libc::dlsym(lib, name.as_ptr()) };
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy(&p) })
}

fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: GL's strings are NUL-terminated.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// The vendor, renderer and version of the OpenGL a surfaceless EGL context
/// gets: desktop OpenGL, as virglrenderer asks for, or OpenGL ES where
/// there is no desktop OpenGL.
fn gl() -> Result<(String, String, String), &'static str> {
    type GetPlatformDisplay = unsafe extern "C" fn(u32, *mut c_void, *const isize) -> *mut c_void;
    type Initialize = unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32;
    type BindApi = unsafe extern "C" fn(u32) -> u32;
    type CreateContext =
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void;
    type MakeCurrent =
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32;
    type GetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;
    type DestroyContext = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;
    type Terminate = unsafe extern "C" fn(*mut c_void) -> u32;
    type GetString = unsafe extern "C" fn(u32) -> *const c_char;

    // SAFETY: each entry point is looked up by name and called as EGL 1.5
    // and OpenGL declare it.
    unsafe {
        let egl = libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if egl.is_null() {
            return Err("libEGL.so.1 is not installed");
        }
        let (
            Some(get_display),
            Some(initialize),
            Some(bind_api),
            Some(create_context),
            Some(make_current),
            Some(get_proc),
            Some(destroy_context),
            Some(terminate),
        ) = (
            sym::<GetPlatformDisplay>(egl, c"eglGetPlatformDisplay"),
            sym::<Initialize>(egl, c"eglInitialize"),
            sym::<BindApi>(egl, c"eglBindAPI"),
            sym::<CreateContext>(egl, c"eglCreateContext"),
            sym::<MakeCurrent>(egl, c"eglMakeCurrent"),
            sym::<GetProcAddress>(egl, c"eglGetProcAddress"),
            sym::<DestroyContext>(egl, c"eglDestroyContext"),
            sym::<Terminate>(egl, c"eglTerminate"),
        )
        else {
            return Err("libEGL.so.1 is missing EGL 1.5's functions");
        };
        let null = std::ptr::null_mut();
        let dpy = get_display(EGL_PLATFORM_SURFACELESS_MESA, null, std::ptr::null());
        if dpy.is_null() {
            return Err("EGL has no surfaceless display");
        }
        let (mut major, mut minor) = (0, 0);
        if initialize(dpy, &mut major, &mut minor) == 0 {
            return Err("EGL did not initialise its surfaceless display");
        }
        let none = [EGL_NONE];
        let mut ctx = null;
        if bind_api(EGL_OPENGL_API) != 0 {
            ctx = create_context(dpy, null, null, none.as_ptr());
        }
        if ctx.is_null() && bind_api(EGL_OPENGL_ES_API) != 0 {
            let es2 = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
            ctx = create_context(dpy, null, null, es2.as_ptr());
        }
        if ctx.is_null() {
            terminate(dpy);
            return Err("EGL made no OpenGL context");
        }
        let out = if make_current(dpy, null, null, ctx) == 0 {
            Err("EGL could not make a context current without a surface")
        } else {
            let gs = get_proc(c"glGetString".as_ptr());
            if gs.is_null() {
                Err("glGetString is missing")
            } else {
                let get_string: GetString = std::mem::transmute(gs);
                Ok((
                    text(get_string(GL_VENDOR)),
                    text(get_string(GL_RENDERER)),
                    text(get_string(GL_VERSION)),
                ))
            }
        };
        make_current(dpy, null, null, null);
        destroy_context(dpy, ctx);
        terminate(dpy);
        out
    }
}

#[repr(C)]
struct InstanceCreateInfo {
    s_type: i32,
    next: *const c_void,
    flags: u32,
    application_info: *const c_void,
    enabled_layer_count: u32,
    enabled_layer_names: *const *const c_char,
    enabled_extension_count: u32,
    enabled_extension_names: *const *const c_char,
}

/// VkPhysicalDeviceProperties begins with apiVersion, driverVersion,
/// vendorID, deviceID and deviceType, four bytes each, then deviceName; the
/// whole structure is well under PROPERTIES_SIZE, whatever its limits hold.
const PROPERTIES_SIZE: usize = 4096;
const DEVICE_TYPE_AT: usize = 16;
const DEVICE_NAME_AT: usize = 20;
const DEVICE_NAME_LEN: usize = 256;
const PHYSICAL_DEVICE_TYPE_CPU: u32 = 4;

/// Vulkan's devices, in the order it lists them.
fn vulkan() -> Result<Vec<ProbedVulkan>, &'static str> {
    type GetInstanceProcAddr = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
    type CreateInstance =
        unsafe extern "C" fn(*const InstanceCreateInfo, *const c_void, *mut *mut c_void) -> i32;
    type EnumeratePhysicalDevices =
        unsafe extern "C" fn(*mut c_void, *mut u32, *mut *mut c_void) -> i32;
    type GetProperties = unsafe extern "C" fn(*mut c_void, *mut u8);
    type DestroyInstance = unsafe extern "C" fn(*mut c_void, *const c_void);

    // SAFETY: each entry point is looked up through the loader and called
    // as vulkan_core.h declares it; the properties are read from a buffer
    // larger than the structure.
    unsafe {
        let vk = libc::dlopen(
            c"libvulkan.so.1".as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        );
        if vk.is_null() {
            return Err("libvulkan.so.1 is not installed");
        }
        let Some(gipa) = sym::<GetInstanceProcAddr>(vk, c"vkGetInstanceProcAddr") else {
            return Err("vkCreateInstance is missing");
        };
        let proc = |instance: *mut c_void, name: &CStr| gipa(instance, name.as_ptr());
        let create = proc(std::ptr::null_mut(), c"vkCreateInstance");
        if create.is_null() {
            return Err("vkCreateInstance is missing");
        }
        let create: CreateInstance = std::mem::transmute(create);
        // VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO.
        let info = InstanceCreateInfo {
            s_type: 1,
            next: std::ptr::null(),
            flags: 0,
            application_info: std::ptr::null(),
            enabled_layer_count: 0,
            enabled_layer_names: std::ptr::null(),
            enabled_extension_count: 0,
            enabled_extension_names: std::ptr::null(),
        };
        let mut instance = std::ptr::null_mut();
        if create(&info, std::ptr::null(), &mut instance) != 0 {
            return Err("Vulkan made no instance");
        }
        let enumerate = proc(instance, c"vkEnumeratePhysicalDevices");
        let properties = proc(instance, c"vkGetPhysicalDeviceProperties");
        let out = if enumerate.is_null() || properties.is_null() {
            Err("vkEnumeratePhysicalDevices is missing")
        } else {
            let enumerate: EnumeratePhysicalDevices = std::mem::transmute(enumerate);
            let properties: GetProperties = std::mem::transmute(properties);
            let mut count = 0u32;
            let mut devices: Vec<*mut c_void> = Vec::new();
            if enumerate(instance, &mut count, std::ptr::null_mut()) != 0 || {
                devices.resize(count.max(1) as usize, std::ptr::null_mut());
                enumerate(instance, &mut count, devices.as_mut_ptr()) != 0
            } {
                Err("Vulkan did not list its devices")
            } else {
                let mut buf = vec![0u8; PROPERTIES_SIZE];
                Ok(devices[..count as usize]
                    .iter()
                    .map(|&d| {
                        buf.fill(0);
                        properties(d, buf.as_mut_ptr());
                        let ty = u32::from_ne_bytes(
                            buf[DEVICE_TYPE_AT..DEVICE_TYPE_AT + 4].try_into().unwrap(),
                        );
                        let name = &buf[DEVICE_NAME_AT..DEVICE_NAME_AT + DEVICE_NAME_LEN];
                        let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                        ProbedVulkan {
                            name: String::from_utf8_lossy(&name[..end]).into_owned(),
                            software: ty == PHYSICAL_DEVICE_TYPE_CPU,
                        }
                    })
                    .collect())
            }
        };
        let destroy = proc(instance, c"vkDestroyInstance");
        if !destroy.is_null() {
            let destroy: DestroyInstance = std::mem::transmute(destroy);
            destroy(instance, std::ptr::null());
        }
        out
    }
}
