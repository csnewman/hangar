//go:build darwin && krun

package main

/*
#include <dlfcn.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

// The few Vulkan structures the check needs, as vulkan_core.h lays them
// out: hangar-krun builds without the Vulkan SDK's headers.
typedef struct {
	int32_t sType;
	const void *pNext;
	const char *pApplicationName;
	uint32_t applicationVersion;
	const char *pEngineName;
	uint32_t engineVersion;
	uint32_t apiVersion;
} hk_application_info;

typedef struct {
	int32_t sType;
	const void *pNext;
	uint32_t flags;
	const hk_application_info *pApplicationInfo;
	uint32_t enabledLayerCount;
	const char *const *ppEnabledLayerNames;
	uint32_t enabledExtensionCount;
	const char *const *ppEnabledExtensionNames;
} hk_instance_create_info;

typedef struct {
	char extensionName[256];
	uint32_t specVersion;
} hk_extension_properties;

typedef void *(*hk_get_instance_proc_addr)(void *instance, const char *name);
typedef int32_t (*hk_create_instance)(const hk_instance_create_info *, const void *, void **);
typedef int32_t (*hk_enumerate_physical_devices)(void *, uint32_t *, void **);
typedef int32_t (*hk_enumerate_device_extensions)(void *, const char *, uint32_t *,
                                                  hk_extension_properties *);
typedef void (*hk_destroy_instance)(void *, const void *);

// hk_device_extensions lists the first Vulkan device's extensions, one per
// line, through the loader beside hangar-krun; NULL if there is none.
static char *hk_device_extensions(void) {
	void *lib = dlopen("@rpath/libvulkan.1.dylib", RTLD_NOW | RTLD_LOCAL);
	if (!lib)
		return NULL;
	hk_get_instance_proc_addr gipa = (hk_get_instance_proc_addr)dlsym(lib, "vkGetInstanceProcAddr");
	if (!gipa)
		return NULL;
	hk_create_instance create = (hk_create_instance)gipa(NULL, "vkCreateInstance");
	if (!create)
		return NULL;

	// VK_STRUCTURE_TYPE_APPLICATION_INFO, _INSTANCE_CREATE_INFO and
	// VK_API_VERSION_1_1.
	hk_application_info app = {0, NULL, "hangar-krun", 0, NULL, 0, (1u << 22) | (1u << 12)};
	hk_instance_create_info info = {1, NULL, 0, &app, 0, NULL, 0, NULL};
	void *instance = NULL;
	if (create(&info, NULL, &instance) != 0)
		return NULL;

	char *out = NULL;
	hk_enumerate_physical_devices enum_devices =
		(hk_enumerate_physical_devices)gipa(instance, "vkEnumeratePhysicalDevices");
	hk_enumerate_device_extensions enum_extensions =
		(hk_enumerate_device_extensions)gipa(instance, "vkEnumerateDeviceExtensionProperties");
	uint32_t n = 1;
	void *device = NULL;
	// VK_INCOMPLETE (5) when there are more devices than the first.
	int32_t r = enum_devices ? enum_devices(instance, &n, &device) : -1;
	if ((r == 0 || r == 5) && n == 1 && enum_extensions) {
		uint32_t count = 0;
		if (enum_extensions(device, NULL, &count, NULL) == 0) {
			hk_extension_properties *props = calloc(count ? count : 1, sizeof(*props));
			if (props && enum_extensions(device, NULL, &count, props) == 0) {
				out = calloc(count, 257);
				if (out) {
					for (uint32_t i = 0; i < count; i++) {
						props[i].extensionName[255] = 0;
						strcat(out, props[i].extensionName);
						strcat(out, "\n");
					}
				}
			}
			free(props);
		}
	}
	hk_destroy_instance destroy = (hk_destroy_instance)gipa(instance, "vkDestroyInstance");
	if (destroy)
		destroy(instance, NULL);
	return out;
}
*/
import "C"

import (
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"unsafe"

	"golang.org/x/sys/unix"

	"github.com/csnewman/hangar/internal/krun"
)

// zinkExtensions are what Mesa's OpenGL on Vulkan (zink) needs of the Mac's
// driver to share a guest program's images with its compositor: images in a
// layout another can read (a DRM format modifier), handed between them as
// dma-bufs, which virglrenderer makes of Metal heaps (dev/krun/*.patch).
var zinkExtensions = []string{
	"VK_EXT_image_drm_format_modifier",
	"VK_EXT_queue_family_foreign",
	"VK_EXT_external_memory_metal",
}

// canZink is whether the Vulkan driver chosen (VK_DRIVER_FILES, set by
// chooseVulkan) has zinkExtensions.
func canZink() bool {
	list := C.hk_device_extensions()
	if list == nil {
		return false
	}
	defer C.free(unsafe.Pointer(list))
	have := strings.Split(C.GoString(list), "\n")
	for _, e := range zinkExtensions {
		if !slices.Contains(have, e) {
			return false
		}
	}
	return true
}

// chooseVulkan picks the Vulkan driver the guest's GPU renders with from
// those bundled beside hangar-krun (bin/vulkan): KosmicKrisp on macOS 26
// and later, which it needs, and MoltenVK otherwise. VK_DRIVER_FILES, set
// already, names another; the loader reads it when virglrenderer starts.
func chooseVulkan() string {
	if f := os.Getenv("VK_DRIVER_FILES"); f != "" {
		if strings.Contains(strings.ToLower(f), "kosmickrisp") {
			return krun.VulkanKosmicKrisp
		}
		return krun.VulkanMoltenVK
	}
	exe, err := os.Executable()
	if err != nil {
		return ""
	}
	dir := filepath.Join(filepath.Dir(exe), "vulkan")
	choice := krun.VulkanMoltenVK
	if v, err := unix.Sysctl("kern.osproductversion"); err == nil {
		if major, _ := strconv.Atoi(strings.SplitN(v, ".", 2)[0]); major >= 26 {
			if _, err := os.Stat(filepath.Join(dir, "kosmickrisp.json")); err == nil {
				choice = krun.VulkanKosmicKrisp
			}
		}
	}
	os.Setenv("VK_DRIVER_FILES", filepath.Join(dir, choice+".json"))
	return choice
}
