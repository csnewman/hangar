// Package qemu builds the QEMU that Hangar runs environments under.
//
// Hangar builds its own rather than using the distribution's for two reasons.
//
// The first is that a distribution QEMU is typically not linked against
// virglrenderer, so it has no virtio-gpu-gl device and cannot accelerate a
// guest's display at all -- Ubuntu 24.04 ships exactly that build. An
// accelerated desktop is the common case, so the capability is not optional.
//
// The second is attack surface. A stock QEMU carries several hundred device
// models, and device emulation is where most QEMU CVEs are. A Hangar guest
// sees a fixed, known set of virtio devices, so everything else is exposure
// with no use. Building with --without-default-devices and an explicit list
// means the binary cannot emulate hardware a guest could never address.
//
// The cost is that QEMU security updates become Hangar's to track rather than
// the distribution's.
package qemu

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
)

// DefaultVersion is the QEMU release Hangar builds.
const DefaultVersion = "11.1.1"

// builderImage matches the mkosi and kernel builders, so one container image
// is pulled and cached rather than three.
const builderImage = "ubuntu:26.04"

// Options controls a build.
type Options struct {
	Version string // upstream QEMU version, e.g. "11.1.1"
	Arch    string // "aarch64" or "x86_64"; defaults to the host
	OutDir  string
	Jobs    int
	Verbose bool
}

// Artifacts are the outputs of a build.
type Artifacts struct {
	Binary  string // qemu-system-<arch>
	Share   string // QEMU's datadir: firmware, keymaps
	Devices string // the device list the binary actually has
	Deps    string // its shared library dependencies
	Version string
}

// HostArch maps GOARCH onto QEMU's architecture names.
func HostArch() (string, error) {
	switch runtime.GOARCH {
	case "arm64":
		return "aarch64", nil
	case "amd64":
		return "x86_64", nil
	default:
		return "", fmt.Errorf("unsupported architecture %q", runtime.GOARCH)
	}
}

// Build compiles QEMU inside a container and writes the result to OutDir.
func Build(ctx context.Context, o Options) (*Artifacts, error) {
	if o.Version == "" {
		o.Version = DefaultVersion
	}
	if o.Arch == "" {
		a, err := HostArch()
		if err != nil {
			return nil, err
		}
		o.Arch = a
	}
	if o.OutDir == "" {
		o.OutDir = "out/qemu"
	}
	if err := os.MkdirAll(o.OutDir, 0o755); err != nil {
		return nil, err
	}

	absOut, err := filepath.Abs(o.OutDir)
	if err != nil {
		return nil, err
	}

	scripts, err := materialiseScripts()
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(scripts)

	cfgDir, err := materialiseConfig()
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(cfgDir)

	// The source tarball is large and the build is slow, so cache the download
	// between runs the way the kernel build does.
	cache, err := os.UserCacheDir()
	if err == nil {
		cache = filepath.Join(cache, "hangar", "qemu-src")
		if err := os.MkdirAll(cache, 0o755); err != nil {
			cache = ""
		}
	} else {
		cache = ""
	}

	args := []string{
		"run", "--rm",
		"-e", "QEMU_VERSION=" + o.Version,
		"-e", "QARCH=" + o.Arch,
		"-e", fmt.Sprintf("OUT_UID=%d", os.Getuid()),
		"-e", fmt.Sprintf("OUT_GID=%d", os.Getgid()),
		"-v", absOut + ":/out",
		"-v", cfgDir + ":/cfg:ro",
		"-v", scripts + ":/scripts:ro",
	}
	if o.Jobs > 0 {
		args = append(args, "-e", fmt.Sprintf("JOBS=%d", o.Jobs))
	}
	if cache != "" {
		args = append(args, "-v", cache+":/cache")
	}
	args = append(args, builderImage, "sh", "/scripts/build.sh")

	if err := run(ctx, o.Verbose, "docker", args...); err != nil {
		return nil, fmt.Errorf("qemu build: %w", err)
	}

	a := &Artifacts{
		Binary:  filepath.Join(o.OutDir, "qemu-system-"+o.Arch),
		Share:   filepath.Join(o.OutDir, "share"),
		Devices: filepath.Join(o.OutDir, "devices"),
		Deps:    filepath.Join(o.OutDir, "deps"),
		Version: o.Version,
	}
	if _, err := os.Stat(a.Binary); err != nil {
		return nil, fmt.Errorf("%s was not produced", a.Binary)
	}
	return a, nil
}

// DeviceCount reports how many device models the built binary carries, for
// comparison against a stock build.
func DeviceCount(devicesFile string) int {
	b, err := os.ReadFile(devicesFile)
	if err != nil {
		return 0
	}
	n := 0
	for _, line := range strings.Split(string(b), "\n") {
		if strings.HasPrefix(line, "name \"") {
			n++
		}
	}
	return n
}
