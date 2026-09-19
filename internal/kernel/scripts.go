package kernel

import (
	"embed"
	"os"
	"path/filepath"
)

//go:embed scripts/build.sh
var scriptFS embed.FS

// hangar.config is the only kernel source we keep -- the kernel tree itself is
// downloaded at build time and never committed. It lives here rather than at
// the repository root because go:embed cannot reach outside the package, and
// one copy is better than two that can drift.
//
//go:embed hangar.config
var configFragment []byte

// The architecture-specific fragments. They are separate files so that the
// build can insist every option a fragment names actually survives
// olddefconfig: an arch's options are simply absent on the other arch, which
// would make that check impossible if both sets shared one file.
//
//go:embed hangar-arm64.config hangar-x86_64.config
var archFragments embed.FS

func materialiseScripts() (string, error) {
	dir, err := os.MkdirTemp("", "hangar-kscripts-")
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

func materialiseConfig() (string, error) {
	dir, err := os.MkdirTemp("", "hangar-kcfg-")
	if err != nil {
		return "", err
	}
	if err := os.WriteFile(filepath.Join(dir, "hangar.config"), configFragment, 0o644); err != nil {
		os.RemoveAll(dir)
		return "", err
	}
	// Both arch fragments go in: the build script picks by KARCH, and shipping
	// the pair keeps this function independent of the target.
	for _, name := range []string{"hangar-arm64.config", "hangar-x86_64.config"} {
		b, err := archFragments.ReadFile(name)
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
