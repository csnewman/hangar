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
	} `yaml:"storage"`

	// Reserved is capacity kept back for the host itself.
	Reserved struct {
		CPUs   int  `yaml:"cpus"`
		Memory Size `yaml:"memory"`
	} `yaml:"reserved"`

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
	Editor    string `yaml:"editor"`
	UpperGiB  int    `yaml:"upper_gib"`
	DockerGiB int    `yaml:"docker_gib"`
	// DaxMiB sizes the DAX window through which environments map their
	// base's files from this machine's page cache. 0 serves the base over
	// virtio-fs without one.
	DaxMiB int `yaml:"dax_mib"`
	// Images maps each image reference a template may name to where this
	// machine fetches it from into its local store.
	Images map[string]VMImage `yaml:"images"`
}

// VMImage is where this machine fetches one image from: its root
// filesystem, a directory, which environments boot from over virtio-fs.
type VMImage struct {
	Base string `yaml:"base"`
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
