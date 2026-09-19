package image

import (
	"embed"
	"fmt"
	"os"
	"path/filepath"
)

// The build steps are real shell files rather than Go string constants: the
// escaping required to embed shell in Go source is how a heredoc delimiter
// silently turned into garbage and produced an image with no init script.
// Kept as files they are also editable and lintable.
//
//go:embed scripts/*.sh
var scriptFS embed.FS

// materialiseScripts writes the embedded scripts to a directory that can be
// bind-mounted into the builder container at /scripts. The caller removes it.
func materialiseScripts() (string, error) {
	dir, err := os.MkdirTemp("", "hangar-scripts-")
	if err != nil {
		return "", err
	}
	entries, err := scriptFS.ReadDir("scripts")
	if err != nil {
		os.RemoveAll(dir)
		return "", err
	}
	for _, e := range entries {
		b, err := scriptFS.ReadFile(filepath.Join("scripts", e.Name()))
		if err != nil {
			os.RemoveAll(dir)
			return "", err
		}
		if err := os.WriteFile(filepath.Join(dir, e.Name()), b, 0o755); err != nil {
			os.RemoveAll(dir)
			return "", fmt.Errorf("writing %s: %w", e.Name(), err)
		}
	}
	return dir, nil
}
