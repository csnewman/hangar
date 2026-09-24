package image

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// BuildAgent compiles hangar-agent for the guest and returns a directory
// holding it, as hangar-agent. The caller removes the directory.
//
// The agent belongs to the node, not to the image: it is each boot's
// initramfs, as init, and then the environment's agent.
//
// CGO is off so the result is static and depends on nothing in the guest --
// no libc version to match, and it runs as init before there is a root
// filesystem at all.
func BuildAgent(ctx context.Context, platform string, verbose bool) (string, error) {
	goos, goarch, ok := strings.Cut(platform, "/")
	if !ok {
		return "", fmt.Errorf("cannot read a GOOS/GOARCH from platform %q", platform)
	}

	dir, err := os.MkdirTemp("", "hangar-agent-")
	if err != nil {
		return "", err
	}

	out := filepath.Join(dir, "hangar-agent")
	cmd := []string{"build", "-trimpath", "-ldflags", "-s -w", "-o", out, "./cmd/hangar-agent"}
	if err := runWithEnv(ctx, verbose, []string{
		"GOOS=" + goos,
		"GOARCH=" + goarch,
		"CGO_ENABLED=0",
	}, "go", cmd...); err != nil {
		os.RemoveAll(dir)
		return "", fmt.Errorf("building hangar-agent for %s: %w", platform, err)
	}
	return dir, nil
}
