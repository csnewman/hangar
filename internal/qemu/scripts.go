package qemu

import (
	"context"
	"embed"
	"io"
	"os"
	"os/exec"
	"path/filepath"
)

//go:embed scripts/build.sh
var scriptFS embed.FS

// The device selection is the only QEMU source Hangar keeps -- the tree
// itself is downloaded at build time. These live here rather than at the
// repository root because go:embed cannot reach outside the package.
//
//go:embed devices.mak devices-aarch64.mak devices-x86_64.mak
var configFS embed.FS

func materialiseScripts() (string, error) {
	dir, err := os.MkdirTemp("", "hangar-qscripts-")
	if err != nil {
		return "", err
	}
	b, err := scriptFS.ReadFile("scripts/build.sh")
	if err != nil {
		os.RemoveAll(dir)
		return "", err
	}
	if err := os.WriteFile(filepath.Join(dir, "build.sh"), b, 0o755); err != nil {
		os.RemoveAll(dir)
		return "", err
	}
	return dir, nil
}

// materialiseConfig writes both architectures' device selections. The build
// script picks by QARCH; shipping the pair keeps this independent of target.
func materialiseConfig() (string, error) {
	dir, err := os.MkdirTemp("", "hangar-qcfg-")
	if err != nil {
		return "", err
	}
	for _, name := range []string{"devices.mak", "devices-aarch64.mak", "devices-x86_64.mak"} {
		b, err := configFS.ReadFile(name)
		if err != nil {
			os.RemoveAll(dir)
			return "", err
		}
		if err := os.WriteFile(filepath.Join(dir, name), b, 0o644); err != nil {
			os.RemoveAll(dir)
			return "", err
		}
	}
	return dir, nil
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
