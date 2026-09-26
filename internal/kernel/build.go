// Package kernel builds Hangar's guest kernel.
//
// The kernel is Hangar's, not the distribution's, for two reasons. It has to
// match the device model we actually use -- virtio for the disks, the
// filesystem and the network, vsock for the agent tunnel -- and everything on
// the boot path has to be built in rather than a module, because the
// initramfs assembles an overlay root before any filesystem exists to load
// modules from. Distribution kernels satisfy neither reliably: Ubuntu ships
// overlayfs as a module and wraps arm64 kernels in an EFI zboot container
// that a direct kernel boot cannot use.
//
// Only the config fragment is source. The kernel tree is downloaded at build
// time and never committed.
package kernel

import (
	"cmp"
	"context"
	_ "embed"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
)

// DefaultVersion is a longterm release, chosen over latest-stable because
// environments are long lived and a kernel CVE should be a point release
// rather than a series bump. It is the file version beside this one, which
// the worker image's build reads too, so the kernel `hangar kernel` builds
// and the one the worker ships are the same.
var DefaultVersion = strings.TrimSpace(versionFile)

//go:embed version
var versionFile string

const builderImage = "ubuntu:26.04"

// Options controls a kernel build.
type Options struct {
	Version string // upstream kernel version, e.g. "6.18.52"
	Arch    string // "arm64" or "x86_64"; defaults to the host
	OutDir  string
	Jobs    int
	Verbose bool

	// Base is the kconfig target the fragment is merged onto. tinyconfig
	// starts from nothing, so anything in the kernel is there because we asked
	// for it.
	Base string

	// ConfigOnly resolves and verifies the config without compiling. Configuring
	// takes seconds and compiling takes the best part of an hour, so this is
	// how the config is iterated on.
	ConfigOnly bool
}

// Artifacts are the outputs of a kernel build.
type Artifacts struct {
	Image         string // raw Image / bzImage, for a direct kernel boot
	Config        string // the resolved .config
	KernelRelease string // e.g. "6.18.52"
}

// Build produces a kernel and its matching modules.
func Build(ctx context.Context, o Options) (*Artifacts, error) {
	if o.Version == "" {
		o.Version = DefaultVersion
	}
	if o.Arch == "" {
		switch runtime.GOARCH {
		case "arm64":
			o.Arch = "arm64"
		case "amd64":
			o.Arch = "x86_64"
		default:
			return nil, fmt.Errorf("unsupported host architecture %q", runtime.GOARCH)
		}
	}
	if o.OutDir == "" {
		o.OutDir = "out/kernel"
	}
	if err := os.MkdirAll(o.OutDir, 0o755); err != nil {
		return nil, err
	}

	if _, err := exec.LookPath("docker"); err != nil {
		return nil, fmt.Errorf("docker not found in PATH; it is used to build the kernel")
	}
	if err := exec.CommandContext(ctx, "docker", "info").Run(); err != nil {
		return nil, fmt.Errorf("docker daemon is not reachable: %w", err)
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

	args := []string{
		"run", "--rm",
		"-e", "KERNEL_VERSION=" + o.Version,
		"-e", "KARCH=" + o.Arch,
		"-e", "KBASE=" + cmp.Or(o.Base, "tinyconfig"),
		"-v", absOut + ":/out",
		"-v", cfgDir + ":/cfg:ro",
		"-v", scripts + ":/scripts:ro",
	}

	// Cache the kernel tarball across runs; iterating on the config should not
	// mean re-downloading it every time.
	if cache, err := os.UserCacheDir(); err == nil {
		kcache := filepath.Join(cache, "hangar", "kernel-src")
		if err := os.MkdirAll(kcache, 0o755); err == nil {
			args = append(args, "-v", kcache+":/cache")
		}
	}
	if o.ConfigOnly {
		args = append(args, "-e", "CONFIG_ONLY=1")
	}
	if o.Jobs > 0 {
		args = append(args, "-e", fmt.Sprintf("JOBS=%d", o.Jobs))
	}
	// The guest kernel must match the guest architecture, which follows the
	// host, so the builder runs on the same platform.
	args = append(args, "--platform", "linux/"+runtime.GOARCH)
	args = append(args, builderImage, "sh", "/scripts/build.sh")

	cmd := exec.CommandContext(ctx, "docker", args...)
	cmd.Stderr = os.Stderr
	if o.Verbose {
		cmd.Stdout = os.Stderr
	}
	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("kernel build: %w", err)
	}

	a := &Artifacts{
		Image:  filepath.Join(o.OutDir, "vmlinuz"),
		Config: filepath.Join(o.OutDir, "config"),
	}
	if o.ConfigOnly {
		return a, nil
	}
	if b, err := os.ReadFile(filepath.Join(o.OutDir, "kernelrelease")); err == nil {
		a.KernelRelease = strings.TrimSpace(string(b))
	}
	for _, p := range []string{a.Image, a.Config} {
		if _, err := os.Stat(p); err != nil {
			return nil, fmt.Errorf("expected artefact missing: %s", p)
		}
	}
	return a, nil
}
