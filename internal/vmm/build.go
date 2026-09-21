package vmm

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
)

// SourceDir is where the monitor's crate lives, relative to the repository
// root.
const SourceDir = "vmm"

// Build compiles the monitor for the machine it is running on.
//
// There is no container here, unlike the kernel and QEMU builds. Those exist
// to pin a toolchain that affects the artefact's behaviour; a Rust binary
// built against the host's libc is the artefact, and a worker that can run
// the monitor can build it.
func Build(ctx context.Context, release bool) (string, error) {
	cargo, err := exec.LookPath("cargo")
	if err != nil {
		home, herr := os.UserHomeDir()
		if herr == nil {
			p := filepath.Join(home, ".cargo", "bin", "cargo")
			if _, serr := os.Stat(p); serr == nil {
				cargo, err = p, nil
			}
		}
		if err != nil {
			return "", fmt.Errorf("cargo not found: install Rust from https://rustup.rs: %w", err)
		}
	}

	args := []string{"build"}
	profile := "debug"
	if release {
		args = append(args, "--release")
		profile = "release"
	}

	cmd := exec.CommandContext(ctx, cargo, args...)
	cmd.Dir = SourceDir
	cmd.Stdout = os.Stderr
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		return "", fmt.Errorf("building the monitor: %w", err)
	}

	out := filepath.Join(SourceDir, "target", profile, "hangar-vmm")
	if _, err := os.Stat(out); err != nil {
		return "", fmt.Errorf("the build left no binary at %s", out)
	}
	return filepath.Abs(out)
}
