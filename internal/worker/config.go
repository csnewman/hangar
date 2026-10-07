package worker

import (
	"errors"
	"fmt"
	"os"
	"strconv"
	"strings"

	"go.yaml.in/yaml/v3"
)

// Config is a worker's YAML file. It holds only what a worker cannot be told
// by the control plane: how to reach it, and facts about this machine. If two
// workers could legitimately disagree about a setting it belongs here;
// anything else is a property of an environment and lives in the database.
type Config struct {
	Server struct {
		URL string `yaml:"url"`
		// TokenFile holds the shared bootstrap token, exchanged once for a
		// credential of this worker's own.
		TokenFile string `yaml:"token_file"`
		// CredentialFile is where the issued credential is kept. It is the
		// worker's identity: lose it and an operator must remove the worker
		// before its name can register again.
		CredentialFile string `yaml:"credential_file"`
	} `yaml:"server"`

	Node struct {
		// Name defaults to the hostname.
		Name   string            `yaml:"name"`
		Labels map[string]string `yaml:"labels"`
	} `yaml:"node"`

	Storage struct {
		Images       string `yaml:"images"`
		Environments string `yaml:"environments"`
		Caches       string `yaml:"caches"`
		// Files holds the files environments share with others -- their
		// owners' profiles and packs -- each file set a directory named by
		// its ID, which each environment's NFS server serves it from. It
		// is the same on every worker: a mount of one NFS share, or EFS.
		// Empty serves none.
		Files string `yaml:"files"`
	} `yaml:"storage"`

	// Reserved is capacity kept back for the host itself.
	Reserved struct {
		CPUs   int  `yaml:"cpus"`
		Memory Size `yaml:"memory"`
	} `yaml:"reserved"`

	// Overcommit multiplies what this machine offers, once Reserved is taken
	// off; unset is 1, offering what there is. vCPUs are time-sliced, so
	// more of them than cores only slows guests when they are busy at once.
	// Memory beyond the machine's is for guests that leave theirs unused:
	// the balloon hands a guest's free memory back to the host, and swap
	// takes what is left, slowly, so memory overcommit wants swap to cover
	// it.
	Overcommit struct {
		CPUs   float64 `yaml:"cpus"`
		Memory float64 `yaml:"memory"`
	} `yaml:"overcommit"`

	// Runtime is what runs environments on this machine: "cloud-hypervisor"
	// runs each as a virtual machine; "simulated" runs nothing and walks
	// each environment through its phases, for developing the control plane
	// on a machine without KVM.
	Runtime string `yaml:"runtime"`

	// VM configures the cloud-hypervisor runtime.
	VM VMConfig `yaml:"vm"`
}

// VMConfig is how this machine runs environments as virtual machines. The
// files named here are the node's, not the environment's: one kernel and one
// agent serve every environment, whatever its image.
type VMConfig struct {
	Kernel string `yaml:"kernel"`
	// Agent is a hangar-agent built for the guest's architecture. It is each
	// environment's initramfs, as init, and then its agent.
	Agent string `yaml:"agent"`
	// Editor is the editor disk editor/build.sh makes for the guest's
	// architecture, attached read-only to every environment here. Without
	// one, environments have no editor.
	Editor string `yaml:"editor"`
	// UpperGiB and DockerGiB are an environment's room for its files and
	// for Docker's store: one sparse disk of their sum.
	UpperGiB  int `yaml:"upper_gib"`
	DockerGiB int `yaml:"docker_gib"`
	// ImageDevice is how an environment's image reaches it: "disk" (the
	// default), an EROFS image as a read-only virtio-blk disk; "pmem", the
	// same image as virtio-pmem, mapped with DAX for an environment that
	// asks; or "virtiofs", the image's directory served over virtio-fs.
	ImageDevice string `yaml:"image_device"`
	// DaxMiB sizes the DAX window through which an environment that asks
	// for DAX maps its base's files from this machine's page cache, with
	// image_device virtiofs. 0 serves every base without one, whatever an
	// environment asks.
	DaxMiB int `yaml:"dax_mib"`
	// GPU is what an environment with a virtual GPU gets on this machine.
	GPU VMGPU `yaml:"gpu"`
	// Images maps an image reference a template may name to a local copy
	// this machine takes it from into its store. Every other reference is
	// pulled from its registry.
	Images map[string]VMImage `yaml:"images"`
	// Registries signs pulls in to registries, by host (ghcr.io,
	// registry-1.docker.io, ...). Others are pulled from anonymously.
	Registries map[string]RegistryAuth `yaml:"registries"`
}

// VMGPU is how this machine gives environments a virtual GPU. Which of them
// can be offered depends on the host's renderer, so it is the machine's to
// say rather than a template's.
type VMGPU struct {
	// Venus offers Vulkan through the host's renderer as well as OpenGL. It
	// needs a virglrenderer built with Venus.
	Venus bool `yaml:"venus"`
	// VenusRestore carries a program's Vulkan state across a suspend, by
	// recording what replaying it takes. Without it a Vulkan program ends
	// on resume, as it would on losing a real GPU. Experimental.
	VenusRestore bool `yaml:"venus_restore"`
	// WindowMiB sizes the window the guest maps GPU resources into, which
	// Venus and zero-copy buffers need. 0 is 512.
	WindowMiB int `yaml:"window_mib"`
	// Renderer is what renders: "auto", a GPU if the host has one and its
	// CPU if not; "hardware", a GPU only, offering no virtual GPU without
	// one; or "software", the CPU always, through Mesa's llvmpipe and
	// lavapipe. Empty is auto.
	Renderer string `yaml:"renderer"`
	// Device is the GPU to render on, as its render node, for a host with
	// more than one: /dev/dri/renderD129. Empty is Mesa's choice.
	Device string `yaml:"device"`
}

// VMImage is a local copy of an image: its root filesystem, a directory,
// which environments boot from (VMConfig.ImageDevice).
type VMImage struct {
	Base string `yaml:"base"`
}

// RegistryAuth is how this machine signs in to a registry.
type RegistryAuth struct {
	Username string `yaml:"username"`
	// PasswordFile holds the password or token, so it stays out of this
	// file.
	PasswordFile string `yaml:"password_file"`
}

// LoadConfig reads and checks a worker's YAML file.
func LoadConfig(path string) (*Config, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var c Config
	dec := yaml.NewDecoder(strings.NewReader(string(b)))
	dec.KnownFields(true)
	if err := dec.Decode(&c); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	if c.Server.URL == "" {
		return nil, fmt.Errorf("%s: server.url is required", path)
	}
	c.Server.URL = strings.TrimRight(c.Server.URL, "/")
	if c.Server.CredentialFile == "" {
		c.Server.CredentialFile = "/var/lib/hangar/worker-credential"
	}
	if c.Node.Name == "" {
		h, err := os.Hostname()
		if err != nil {
			return nil, fmt.Errorf("node.name is unset and the hostname is unavailable: %w", err)
		}
		c.Node.Name = h
	}
	if c.Overcommit.CPUs < 0 || (c.Overcommit.CPUs > 0 && c.Overcommit.CPUs < 1) ||
		c.Overcommit.Memory < 0 || (c.Overcommit.Memory > 0 && c.Overcommit.Memory < 1) {
		return nil, errors.New(path + ": overcommit ratios are 1 or more; Reserved keeps capacity back")
	}
	switch c.Runtime {
	case "":
		return nil, errors.New(path + ": runtime is required")
	case "cloud-hypervisor":
		if c.VM.Kernel == "" {
			return nil, errors.New(path + ": vm.kernel is required for the cloud-hypervisor runtime")
		}
		if c.Storage.Environments == "" || c.Storage.Images == "" {
			return nil, errors.New(path + ": storage.environments and storage.images are required for the cloud-hypervisor runtime")
		}
	}
	return &c, nil
}

// Size is an amount of memory in bytes, written in YAML as a plain number of
// bytes or with a binary suffix: 512MiB, 4GiB.
type Size int64

func (s *Size) UnmarshalYAML(n *yaml.Node) error {
	v, err := ParseSize(n.Value)
	if err != nil {
		return err
	}
	*s = v
	return nil
}

// MiB is the size in whole mebibytes, rounded down.
func (s Size) MiB() int { return int(s >> 20) }

// ParseSize reads a size such as "4GiB", "512MiB" or "1073741824".
func ParseSize(v string) (Size, error) {
	v = strings.TrimSpace(v)
	mult := int64(1)
	for _, u := range []struct {
		suffix string
		mult   int64
	}{{"KiB", 1 << 10}, {"MiB", 1 << 20}, {"GiB", 1 << 30}, {"TiB", 1 << 40}} {
		if num, ok := strings.CutSuffix(v, u.suffix); ok {
			v, mult = strings.TrimSpace(num), u.mult
			break
		}
	}
	n, err := strconv.ParseInt(v, 10, 64)
	if err != nil || n < 0 {
		return 0, fmt.Errorf("invalid size %q: want a number of bytes, or KiB, MiB, GiB or TiB", v)
	}
	return Size(n * mult), nil
}
