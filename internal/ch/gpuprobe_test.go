package ch_test

import (
	"slices"
	"strings"
	"testing"

	"github.com/csnewman/hangar/internal/ch"
)

// The renderer a worker asks for becomes the GPU backend's environment:
// surfaceless always, Mesa's software rasterisers for software, and
// nothing that could pick a device for auto or hardware.
func TestGPUEnv(t *testing.T) {
	for _, r := range []string{"", ch.RendererAuto, ch.RendererHardware} {
		env, err := ch.GPUEnv(r, "")
		if err != nil || !slices.Equal(env, []string{"EGL_PLATFORM=surfaceless"}) {
			t.Errorf("renderer %q: %v, %v", r, env, err)
		}
	}
	env, err := ch.GPUEnv(ch.RendererSoftware, "")
	if err != nil || !slices.Contains(env, "LIBGL_ALWAYS_SOFTWARE=1") || !slices.Contains(env, "EGL_PLATFORM=surfaceless") {
		t.Errorf("software: %v, %v", env, err)
	}
	for _, e := range env {
		if strings.HasPrefix(e, "VK_DRIVER_FILES=") && !strings.Contains(e, "lvp_icd") {
			t.Errorf("software's Vulkan driver is %s, not lavapipe", e)
		}
	}
	if _, err := ch.GPUEnv("fastest", ""); err == nil {
		t.Error("an unknown renderer was taken")
	}
	if _, err := ch.GPUEnv(ch.RendererSoftware, "/dev/dri/renderD128"); err == nil {
		t.Error("a device was taken for software rendering")
	}
	if _, err := ch.GPUEnv(ch.RendererAuto, "/dev/dri/renderD999"); err == nil {
		t.Error("a device that does not exist was taken")
	}
}
