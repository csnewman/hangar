// Package image turns an mkosi configuration into an image: its root
// filesystem, as a directory, which a worker exports to its environments over
// virtio-fs.
//
// An image carries nothing else. The kernel, the initramfs and the agent are
// the node's; an environment's writable layer and Docker disk are made by the
// worker that runs it.
//
// The build runs in a throwaway builder container, and its output keeps the
// image's owners and modes, so it must be written to a Linux filesystem.
package image

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
)

// Tiers are the images built for each distribution, in build order: each is
// built on the tree of the one before it.
var Tiers = []string{"minimal", "base"}

// Artifacts are the outputs of a build.
type Artifacts struct {
	// Rootfs is each tier's root filesystem, by tier.
	Rootfs map[string]string
	Tag    string // what was built
}

// Options controls a build.
type Options struct {
	ContextDir string // a distribution's directory, holding a directory per tier
	OutDir     string
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
