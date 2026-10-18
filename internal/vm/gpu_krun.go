package vm

import (
	"context"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/krun"
)

// krunGPU is what a machine under libkrun renders with: Vulkan through
// Venus on the Mac's own GPU, by the driver hangar-krun chose, when its
// libkrun was built with a GPU, and nothing otherwise. The GPU has no OpenGL
// of its own (virgl): zink, Mesa's OpenGL on Vulkan, draws it in the guest
// where the driver can share images between the guest's programs, which
// zink reports, and the guest's CPU draws it otherwise.
func krunGPU(ctx context.Context, cfg Config) (info *api.GPUInfo, zink bool) {
	f, err := krun.ReadFeatures(ctx)
	if err != nil {
		cfg.Log.Warn("environments are offered no virtual GPU", "err", err)
		return nil, false
	}
	if !f.GPU {
		cfg.Log.Info("hangar-krun's libkrun was built without a GPU: environments are offered none")
		return nil, false
	}
	driver := "MoltenVK"
	if f.Vulkan == krun.VulkanKosmicKrisp {
		driver = "KosmicKrisp"
	}
	device := macGPUName() + " (" + driver + ")"
	info = &api.GPUInfo{Renderer: "hardware", Vulkan: []api.GPUDevice{{Name: device}}}
	if f.Zink {
		info.GL = &api.GPURenderer{Vendor: "Mesa", Renderer: "zink on " + device}
	} else {
		cfg.Log.Info("OpenGL in environments is drawn on their CPU: " + driver +
			" cannot share images between a guest's programs (dev/krun-build.sh builds a KosmicKrisp that can)")
		info.GL = &api.GPURenderer{Vendor: "Mesa", Renderer: "llvmpipe (the guest's CPU)", Software: true}
	}
	return info, f.Zink
}
