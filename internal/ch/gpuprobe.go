package ch

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

// Renderers a worker may ask its GPU backends for: whatever Mesa finds
// (a GPU if there is one, the CPU if not), a GPU only, or the CPU only.
const (
	RendererAuto     = "auto"
	RendererHardware = "hardware"
	RendererSoftware = "software"
)

// GPUEnv is the environment a GPU backend runs with to render as asked:
// on the surfaceless platform always, which a host without /dev/dri needs;
// on the CPU for software, through Mesa's llvmpipe and lavapipe; and on
// device, a render node such as /dev/dri/renderD129, when one is named.
func GPUEnv(renderer, device string) ([]string, error) {
	env := []string{"EGL_PLATFORM=surfaceless"}
	switch renderer {
	case "", RendererAuto, RendererHardware:
	case RendererSoftware:
		if device != "" {
			return nil, fmt.Errorf("a device is for rendering on a GPU, and the renderer is software")
		}
		env = append(env, "LIBGL_ALWAYS_SOFTWARE=1")
		if icd := lavapipeICD(); icd != "" {
			env = append(env, "VK_DRIVER_FILES="+icd)
		}
		return env, nil
	default:
		return nil, fmt.Errorf("renderer %q: want auto, hardware or software", renderer)
	}
	if device != "" {
		sel, err := deviceSelection(device)
		if err != nil {
			return nil, err
		}
		env = append(env, sel...)
	}
	return env, nil
}

// lavapipeICD is lavapipe's Vulkan driver manifest, where the host's
// packages put it, or "" if it has none.
func lavapipeICD() string {
	for _, dir := range []string{"/usr/share/vulkan/icd.d", "/usr/local/share/vulkan/icd.d", "/etc/vulkan/icd.d"} {
		if m, _ := filepath.Glob(filepath.Join(dir, "lvp_icd*.json")); len(m) > 0 {
			return m[0]
		}
	}
	return ""
}

// deviceSelection points Mesa at one GPU, for OpenGL by its PCI address
// (DRI_PRIME) and for Vulkan by its vendor and device IDs, with the others
// hidden (MESA_VK_DEVICE_SELECT's "!").
func deviceSelection(device string) ([]string, error) {
	node := filepath.Base(device)
	sys := filepath.Join("/sys/class/drm", node, "device")
	target, err := filepath.EvalSymlinks(sys)
	if err != nil {
		return nil, fmt.Errorf("GPU device %s: %w", device, err)
	}
	read := func(name string) (string, error) {
		b, err := os.ReadFile(filepath.Join(target, name))
		return strings.TrimPrefix(strings.TrimSpace(string(b)), "0x"), err
	}
	vendor, err := read("vendor")
	if err != nil {
		return nil, fmt.Errorf("GPU device %s is not a PCI device: %w", device, err)
	}
	id, err := read("device")
	if err != nil {
		return nil, fmt.Errorf("GPU device %s: %w", device, err)
	}
	// A PCI address such as 0000:02:00.0, written as Mesa's id_path_tag.
	addr := filepath.Base(target)
	tag := "pci-" + strings.NewReplacer(":", "_", ".", "_").Replace(addr)
	return []string{"DRI_PRIME=" + tag, "MESA_VK_DEVICE_SELECT=" + vendor + ":" + id + "!"}, nil
}

// GPUProbe is what the GPU backend reports it renders with, under the
// environment it was given.
type GPUProbe struct {
	GL *struct {
		Vendor   string `json:"vendor"`
		Renderer string `json:"renderer"`
		Version  string `json:"version"`
		Software bool   `json:"software"`
	} `json:"gl"`
	GLError string `json:"gl_error"`
	Vulkan  []struct {
		Name     string `json:"name"`
		Software bool   `json:"software"`
	} `json:"vulkan"`
	VulkanError string `json:"vulkan_error"`
}

// ProbeGPU asks the GPU backend what it renders with, under env.
func ProbeGPU(ctx context.Context, env []string) (GPUProbe, error) {
	bin, err := FindGpuBackend()
	if err != nil {
		return GPUProbe{}, err
	}
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, bin, "--probe")
	cmd.Env = append(os.Environ(), env...)
	out, err := cmd.Output()
	if err != nil {
		return GPUProbe{}, fmt.Errorf("probing the GPU backend: %w", err)
	}
	var p GPUProbe
	if err := json.Unmarshal(out, &p); err != nil {
		return GPUProbe{}, fmt.Errorf("the GPU backend's probe: %w", err)
	}
	return p, nil
}

// pidOf is a started command's process ID, or 0.
func pidOf(cmd *exec.Cmd) int {
	if cmd == nil || cmd.Process == nil {
		return 0
	}
	return cmd.Process.Pid
}
