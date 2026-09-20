package image

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// buildAgent compiles hangar-agent for the guest and returns a directory
// holding it, for the builder container to mount at /agent.
//
// The agent is compiled here rather than installed from a package because it
// belongs to the node, not to the image: an environment built from an
// arbitrary Dockerfile gets the same agent as a Hangar base, and shipping a
// new agent does not mean rebuilding every image.
//
// CGO is off so the result is static and depends on nothing in the guest --
// no libc version to match, and it runs in an image that has no shared
// libraries at all.
func buildAgent(ctx context.Context, platform string, verbose bool) (string, error) {
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
