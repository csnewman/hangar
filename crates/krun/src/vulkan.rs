//! The Mac's Vulkan driver, bundled beside hangar-krun (bin/vulkan): which
//! one a guest's GPU renders with, and whether it can carry zink.

use std::ffi::{CStr, c_char, c_void};

use hangar_api::monitor::{VULKAN_KOSMICKRISP, VULKAN_MOLTENVK};

/// What Mesa's OpenGL on Vulkan (zink) needs of the Mac's driver to share a
/// guest program's images with its compositor: images in a layout another
/// can read (a DRM format modifier), handed between them as dma-bufs, which
/// virglrenderer makes of Metal heaps (dev/krun/*.patch).
const ZINK_EXTENSIONS: [&str; 3] = [
    "VK_EXT_image_drm_format_modifier",
    "VK_EXT_queue_family_foreign",
    "VK_EXT_external_memory_metal",
];

/// Picks the driver: KosmicKrisp on macOS 26 and later, which it needs,
/// and MoltenVK otherwise. VK_DRIVER_FILES, set already, names another; the
/// loader reads it when virglrenderer starts. Called before any thread.
pub fn choose() -> String {
    if let Ok(f) = std::env::var("VK_DRIVER_FILES")
        && !f.is_empty()
    {
        return if f.to_lowercase().contains("kosmickrisp") {
            VULKAN_KOSMICKRISP
        } else {
            VULKAN_MOLTENVK
        }
        .into();
    }
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.join("vulkan")))
    else {
        return String::new();
    };
    let choice = if macos_major() >= 26 && dir.join("kosmickrisp.json").is_file() {
        VULKAN_KOSMICKRISP
    } else {
        VULKAN_MOLTENVK
    };
    // SAFETY: called from main before any thread starts.
    unsafe {
        std::env::set_var("VK_DRIVER_FILES", dir.join(format!("{choice}.json")));
    }
    choice.into()
}

fn macos_major() -> u32 {
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    // SAFETY: sysctlbyname writes at most len bytes into buf.
    let r = unsafe {
        libc::sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if r != 0 {
        return 0;
    }
    let v = String::from_utf8_lossy(&buf[..len]);
    v.trim_end_matches('\0')
        .split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .unwrap_or(0)
}

/// Whether the chosen driver has ZINK_EXTENSIONS.
pub fn can_zink() -> bool {
    let Some(have) = device_extensions() else {
        return false;
    };
    ZINK_EXTENSIONS.iter().all(|e| have.iter().any(|h| h == e))
}

/// The few Vulkan structures the check needs, as vulkan_core.h lays them
/// out: hangar-krun builds without the Vulkan SDK's headers.
#[repr(C)]
struct ApplicationInfo {
    s_type: i32,
    next: *const c_void,
    application_name: *const c_char,
    application_version: u32,
    engine_name: *const c_char,
    engine_version: u32,
    api_version: u32,
}

#[repr(C)]
struct InstanceCreateInfo {
    s_type: i32,
    next: *const c_void,
    flags: u32,
    application_info: *const ApplicationInfo,
    enabled_layer_count: u32,
    enabled_layer_names: *const *const c_char,
    enabled_extension_count: u32,
    enabled_extension_names: *const *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ExtensionProperties {
    name: [c_char; 256],
    spec_version: u32,
}

type GetInstanceProcAddr = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type CreateInstance =
    unsafe extern "C" fn(*const InstanceCreateInfo, *const c_void, *mut *mut c_void) -> i32;
type EnumeratePhysicalDevices =
    unsafe extern "C" fn(*mut c_void, *mut u32, *mut *mut c_void) -> i32;
type EnumerateDeviceExtensions =
    unsafe extern "C" fn(*mut c_void, *const c_char, *mut u32, *mut ExtensionProperties) -> i32;
type DestroyInstance = unsafe extern "C" fn(*mut c_void, *const c_void);

/// The first Vulkan device's extensions, through the loader beside
/// hangar-krun; none if there is no device.
fn device_extensions() -> Option<Vec<String>> {
    // SAFETY: the loader is opened, its entry points looked up by name and
    // called as vulkan_core.h declares them, and the instance destroyed.
    unsafe {
        let lib = libc::dlopen(
            c"@rpath/libvulkan.1.dylib".as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        );
        if lib.is_null() {
            return None;
        }
        let gipa = libc::dlsym(lib, c"vkGetInstanceProcAddr".as_ptr());
        if gipa.is_null() {
            return None;
        }
        let gipa: GetInstanceProcAddr = std::mem::transmute(gipa);
        let proc = |instance: *mut c_void, name: &CStr| gipa(instance, name.as_ptr());
        let create = proc(std::ptr::null_mut(), c"vkCreateInstance");
        if create.is_null() {
            return None;
        }
        let create: CreateInstance = std::mem::transmute(create);
        // VK_STRUCTURE_TYPE_APPLICATION_INFO, _INSTANCE_CREATE_INFO and
        // VK_API_VERSION_1_1.
        let app = ApplicationInfo {
            s_type: 0,
            next: std::ptr::null(),
            application_name: c"hangar-krun".as_ptr(),
            application_version: 0,
            engine_name: std::ptr::null(),
            engine_version: 0,
            api_version: (1 << 22) | (1 << 12),
        };
        let info = InstanceCreateInfo {
            s_type: 1,
            next: std::ptr::null(),
            flags: 0,
            application_info: &app,
            enabled_layer_count: 0,
            enabled_layer_names: std::ptr::null(),
            enabled_extension_count: 0,
            enabled_extension_names: std::ptr::null(),
        };
        let mut instance = std::ptr::null_mut();
        if create(&info, std::ptr::null(), &mut instance) != 0 {
            return None;
        }
        let mut out = None;
        let devices = proc(instance, c"vkEnumeratePhysicalDevices");
        let extensions = proc(instance, c"vkEnumerateDeviceExtensionProperties");
        if !devices.is_null() && !extensions.is_null() {
            let devices: EnumeratePhysicalDevices = std::mem::transmute(devices);
            let extensions: EnumerateDeviceExtensions = std::mem::transmute(extensions);
            let mut n = 1u32;
            let mut device = std::ptr::null_mut();
            // VK_INCOMPLETE (5) when there are more devices than the first.
            let r = devices(instance, &mut n, &mut device);
            if (r == 0 || r == 5) && n == 1 {
                let mut count = 0u32;
                if extensions(device, std::ptr::null(), &mut count, std::ptr::null_mut()) == 0 {
                    let mut props = vec![
                        ExtensionProperties {
                            name: [0; 256],
                            spec_version: 0
                        };
                        count as usize
                    ];
                    if extensions(device, std::ptr::null(), &mut count, props.as_mut_ptr()) == 0 {
                        props.truncate(count as usize);
                        out = Some(
                            props
                                .iter()
                                .map(|p| {
                                    let mut name = p.name;
                                    name[255] = 0;
                                    CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned()
                                })
                                .collect(),
                        );
                    }
                }
            }
        }
        let destroy = proc(instance, c"vkDestroyInstance");
        if !destroy.is_null() {
            let destroy: DestroyInstance = std::mem::transmute(destroy);
            destroy(instance, std::ptr::null());
        }
        out
    }
}
