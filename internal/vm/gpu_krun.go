package vm

import (
	"context"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/krun"
)

// krunGPU is what a machine under libkrun renders with: Vulkan through
// Venus on the Mac's own GPU (MoltenVK), when hangar-krun's libkrun was
// built with a GPU, and nothing otherwise. It has no OpenGL of its own.
func krunGPU(ctx context.Context, cfg Config) *api.GPUInfo {
	f, err := krun.ReadFeatures(ctx)
	if err != nil {
		cfg.Log.Warn("environments are offered no virtual GPU", "err", err)
		return nil
	}
	if !f.GPU {
		cfg.Log.Info("hangar-krun's libkrun was built without a GPU: environments are offered none")
		return nil
	}
	return &api.GPUInfo{Renderer: "hardware", Vulkan: []api.GPUDevice{{Name: macGPUName() + " (MoltenVK)"}}}
}
