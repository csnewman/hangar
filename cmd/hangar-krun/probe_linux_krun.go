//go:build linux && krun

package main

/*
#cgo LDFLAGS: -ldl
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define HK_EGL_PLATFORM_SURFACELESS_MESA 0x31DD
#define HK_EGL_OPENGL_API 0x30A2
#define HK_EGL_OPENGL_ES_API 0x30A0
#define HK_EGL_CONTEXT_CLIENT_VERSION 0x3098
#define HK_EGL_NONE 0x3038
#define HK_GL_VENDOR 0x1F00
#define HK_GL_RENDERER 0x1F01
#define HK_GL_VERSION 0x1F02

typedef void *(*hk_egl_get_platform_display)(uint32_t, void *, const intptr_t *);
typedef uint32_t (*hk_egl_initialize)(void *, int32_t *, int32_t *);
typedef uint32_t (*hk_egl_bind_api)(uint32_t);
typedef void *(*hk_egl_create_context)(void *, void *, void *, const int32_t *);
typedef uint32_t (*hk_egl_make_current)(void *, void *, void *, void *);
typedef void *(*hk_egl_get_proc_address)(const char *);
typedef uint32_t (*hk_egl_destroy_context)(void *, void *);
typedef uint32_t (*hk_egl_terminate)(void *);
typedef const char *(*hk_gl_get_string)(uint32_t);

// hk_gl writes the vendor, renderer and version of the OpenGL a surfaceless
// EGL context gets, each into n bytes: desktop OpenGL, as virglrenderer asks
// for, or OpenGL ES where there is no desktop OpenGL. It returns NULL, or
// why there is none.
static const char *hk_gl(char *vendor, char *renderer, char *version, size_t n) {
	void *egl = dlopen("libEGL.so.1", RTLD_NOW | RTLD_LOCAL);
	if (!egl)
		return "libEGL.so.1 is not installed";
	hk_egl_get_platform_display get_display = (hk_egl_get_platform_display)dlsym(egl, "eglGetPlatformDisplay");
	hk_egl_initialize initialize = (hk_egl_initialize)dlsym(egl, "eglInitialize");
	hk_egl_bind_api bind_api = (hk_egl_bind_api)dlsym(egl, "eglBindAPI");
	hk_egl_create_context create_context = (hk_egl_create_context)dlsym(egl, "eglCreateContext");
	hk_egl_make_current make_current = (hk_egl_make_current)dlsym(egl, "eglMakeCurrent");
	hk_egl_get_proc_address get_proc = (hk_egl_get_proc_address)dlsym(egl, "eglGetProcAddress");
	hk_egl_destroy_context destroy_context = (hk_egl_destroy_context)dlsym(egl, "eglDestroyContext");
	hk_egl_terminate terminate = (hk_egl_terminate)dlsym(egl, "eglTerminate");
	if (!get_display || !initialize || !bind_api || !create_context || !make_current || !get_proc ||
	    !destroy_context || !terminate)
		return "libEGL.so.1 is missing EGL 1.5's functions";

	void *dpy = get_display(HK_EGL_PLATFORM_SURFACELESS_MESA, NULL, NULL);
	if (!dpy)
		return "EGL has no surfaceless display";
	int32_t major, minor;
	if (!initialize(dpy, &major, &minor))
		return "EGL did not initialise its surfaceless display";
	void *ctx = NULL;
	const int32_t none[] = {HK_EGL_NONE};
	if (bind_api(HK_EGL_OPENGL_API))
		ctx = create_context(dpy, NULL, NULL, none);
	if (!ctx && bind_api(HK_EGL_OPENGL_ES_API)) {
		const int32_t es2[] = {HK_EGL_CONTEXT_CLIENT_VERSION, 2, HK_EGL_NONE};
		ctx = create_context(dpy, NULL, NULL, es2);
	}
	if (!ctx) {
		terminate(dpy);
		return "EGL made no OpenGL context";
	}
	const char *err = NULL;
	hk_gl_get_string get_string = NULL;
	if (!make_current(dpy, NULL, NULL, ctx))
		err = "EGL could not make a context current without a surface";
	else if (!(get_string = (hk_gl_get_string)get_proc("glGetString")))
		err = "glGetString is missing";
	else {
		const char *v = get_string(HK_GL_VENDOR), *r = get_string(HK_GL_RENDERER),
			   *ver = get_string(HK_GL_VERSION);
		snprintf(vendor, n, "%s", v ? v : "");
		snprintf(renderer, n, "%s", r ? r : "");
		snprintf(version, n, "%s", ver ? ver : "");
	}
	make_current(dpy, NULL, NULL, NULL);
	destroy_context(dpy, ctx);
	terminate(dpy);
	return err;
}

typedef struct {
	int32_t sType;
	const void *pNext;
	uint32_t flags;
	const void *pApplicationInfo;
	uint32_t enabledLayerCount;
	const char *const *ppEnabledLayerNames;
	uint32_t enabledExtensionCount;
	const char *const *ppEnabledExtensionNames;
} hk_instance_create_info;

typedef void *(*hk_get_instance_proc_addr)(void *, const char *);
typedef int32_t (*hk_create_instance)(const hk_instance_create_info *, const void *, void **);
typedef int32_t (*hk_enumerate_physical_devices)(void *, uint32_t *, void **);
typedef void (*hk_get_physical_device_properties)(void *, uint8_t *);
typedef void (*hk_destroy_instance)(void *, const void *);

// VkPhysicalDeviceProperties begins with apiVersion, driverVersion,
// vendorID, deviceID and deviceType, four bytes each, then deviceName; the
// whole structure is well under HK_PROPERTIES_SIZE, whatever its limits
// hold.
#define HK_PROPERTIES_SIZE 4096
#define HK_DEVICE_TYPE_AT 16
#define HK_DEVICE_NAME_AT 20
#define HK_DEVICE_NAME_LEN 256
#define HK_PHYSICAL_DEVICE_TYPE_CPU 4

// hk_vulkan lists Vulkan's devices, in the order it lists them, one per
// line, each its type ('c' for a CPU device, '-' otherwise) and then its
// name, into a string the caller frees; NULL with *err saying why when it
// cannot.
static char *hk_vulkan(const char **err) {
	void *vk = dlopen("libvulkan.so.1", RTLD_NOW | RTLD_LOCAL);
	if (!vk) {
		*err = "libvulkan.so.1 is not installed";
		return NULL;
	}
	hk_get_instance_proc_addr gipa = (hk_get_instance_proc_addr)dlsym(vk, "vkGetInstanceProcAddr");
	hk_create_instance create = gipa ? (hk_create_instance)gipa(NULL, "vkCreateInstance") : NULL;
	if (!create) {
		*err = "vkCreateInstance is missing";
		return NULL;
	}
	// VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO.
	hk_instance_create_info info = {1, NULL, 0, NULL, 0, NULL, 0, NULL};
	void *instance = NULL;
	if (create(&info, NULL, &instance) != 0) {
		*err = "Vulkan made no instance";
		return NULL;
	}
	hk_enumerate_physical_devices enumerate =
		(hk_enumerate_physical_devices)gipa(instance, "vkEnumeratePhysicalDevices");
	hk_get_physical_device_properties properties =
		(hk_get_physical_device_properties)gipa(instance, "vkGetPhysicalDeviceProperties");
	hk_destroy_instance destroy = (hk_destroy_instance)gipa(instance, "vkDestroyInstance");
	char *out = NULL;
	uint32_t count = 0;
	void **devices = NULL;
	uint8_t *buf = NULL;
	if (!enumerate || !properties) {
		*err = "vkEnumeratePhysicalDevices is missing";
	} else if (enumerate(instance, &count, NULL) != 0 ||
		   !(devices = calloc(count ? count : 1, sizeof(void *))) ||
		   enumerate(instance, &count, devices) != 0) {
		*err = "Vulkan did not list its devices";
	} else if ((out = calloc(count * (HK_DEVICE_NAME_LEN + 2) + 1, 1)) &&
		   (buf = malloc(HK_PROPERTIES_SIZE))) {
		for (uint32_t i = 0; i < count; i++) {
			memset(buf, 0, HK_PROPERTIES_SIZE);
			properties(devices[i], buf);
			uint32_t type;
			memcpy(&type, buf + HK_DEVICE_TYPE_AT, sizeof(type));
			char *name = (char *)buf + HK_DEVICE_NAME_AT;
			name[HK_DEVICE_NAME_LEN - 1] = 0;
			strcat(out, type == HK_PHYSICAL_DEVICE_TYPE_CPU ? "c" : "-");
			strcat(out, name);
			strcat(out, "\n");
		}
	}
	free(buf);
	free(devices);
	if (destroy)
		destroy(instance, NULL);
	return out;
}
*/
import "C"

import (
	"strings"
	"unsafe"

	"github.com/csnewman/hangar/internal/ch"
)

func init() { probe = probeGPU }

// probeGPU is what this host renders with, as libkrun's virglrenderer
// renders on it: the OpenGL renderer EGL gives on the surfaceless
// platform, and the Vulkan devices Venus's render server would choose
// among. It runs with the environment the worker gives a machine, so the
// worker's choice of renderer and device, made through Mesa's environment
// variables, shows here as it takes effect.
func probeGPU() ch.GPUProbe {
	var p ch.GPUProbe
	const n = 256
	vendor, renderer, version := (*C.char)(C.calloc(n, 1)), (*C.char)(C.calloc(n, 1)), (*C.char)(C.calloc(n, 1))
	defer C.free(unsafe.Pointer(vendor))
	defer C.free(unsafe.Pointer(renderer))
	defer C.free(unsafe.Pointer(version))
	if err := C.hk_gl(vendor, renderer, version, n); err != nil {
		p.GLError = C.GoString(err)
	} else {
		r := C.GoString(renderer)
		lower := strings.ToLower(r)
		p.GL = &ch.ProbedGL{
			Vendor:   C.GoString(vendor),
			Renderer: r,
			Version:  C.GoString(version),
			Software: strings.Contains(lower, "llvmpipe") || strings.Contains(lower, "softpipe") ||
				strings.Contains(lower, "swrast"),
		}
	}
	var errC *C.char
	list := C.hk_vulkan(&errC)
	if list == nil {
		p.VulkanError = C.GoString(errC)
		return p
	}
	defer C.free(unsafe.Pointer(list))
	for _, line := range strings.Split(strings.TrimSuffix(C.GoString(list), "\n"), "\n") {
		if line == "" {
			continue
		}
		p.Vulkan = append(p.Vulkan, ch.ProbedVulkan{Name: line[1:], Software: line[0] == 'c'})
	}
	return p
}
