// Package image turns an mkosi configuration into the artefacts a VM needs:
// a read-only ext4 base, an empty writable layer, a filesystem for the Docker
// image store, and an initramfs that stacks them.
//
// This flattens the rootfs to raw disks. The eventual design (see
// docs/image-runtime-format.md) reuses containerd's snapshotter and shares the
// result over virtiofs, which needs a Linux host; raw disks prove the boot path
// and nested Docker on any host.
//
// Everything needing a Linux userspace -- untar with ownership preserved,
// mke2fs -- happens inside a throwaway builder container, so the host needs no
// privileged operations or loop mounts.
package image

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
)

// Artifacts are the outputs of a build.
type Artifacts struct {
	Base   string // read-only lower layer
	Upper  string // empty writable upper layer, one per environment
	Docker string // docker image store, kept off the overlay
	Initrd string // assembles the overlay root and switch_roots
	Tag    string // the OCI tag that was built
}

// Options controls a build.
type Options struct {
	ContextDir string // directory containing mkosi.conf
	OutDir     string
	SizeGB     int
	UpperGB    int
	DockerGB   int
	Platform   string // e.g. "linux/arm64"; empty means the daemon default
	Verbose    bool
}

func requireDocker(ctx context.Context) error {
	if _, err := exec.LookPath("docker"); err != nil {
		return fmt.Errorf("docker not found in PATH; it is used to build images")
	}
	if err := exec.CommandContext(ctx, "docker", "info").Run(); err != nil {
		return fmt.Errorf("docker daemon is not reachable: %w", err)
	}
	return nil
}

// runWithEnv is run with extra environment variables, for cross-compilation.
func runWithEnv(ctx context.Context, verbose bool, env []string, name string, args ...string) error {
	cmd := exec.CommandContext(ctx, name, args...)
	cmd.Env = append(os.Environ(), env...)
	if verbose {
		cmd.Stdout = os.Stderr
	} else {
		cmd.Stdout = io.Discard
	}
	cmd.Stderr = os.Stderr
	return cmd.Run()
}

func run(ctx context.Context, verbose bool, name string, args ...string) error {
	cmd := exec.CommandContext(ctx, name, args...)
	if verbose {
		cmd.Stdout = os.Stderr
	} else {
		cmd.Stdout = io.Discard
	}
	cmd.Stderr = os.Stderr
	return cmd.Run()
}
