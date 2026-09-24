package image

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
)

// BuildWithMkosi builds a base image from an mkosi configuration.
//
// mkosi assembles an OS image from distribution packages, which matters for two
// things a VM needs and a container image does not:
//
//   - udev. On Debian/Ubuntu it is a separate package from systemd, and a guest
//     without it has no working network at all (see docs/dev.md).
//   - no container markers, so systemd boots the guest as a machine instead of
//     reporting "Detected virtualization docker" and running in container mode.
//
// It runs inside a throwaway container needing mount and namespace
// permissions: CAP_SYS_ADMIN with Docker's seccomp and AppArmor profiles
// disabled. Full --privileged is not required, and no Docker socket is mounted.
// This is a trusted base-image build, not a sandbox for user builds. On a Linux
// build host mkosi runs directly and no container is involved.
const mkosiBuilderImage = "ubuntu:26.04"

func BuildWithMkosi(ctx context.Context, o Options) (*Artifacts, error) {
	if err := os.MkdirAll(o.OutDir, 0o755); err != nil {
		return nil, err
	}
	if err := requireDocker(ctx); err != nil {
		return nil, err
	}

	absOut, err := filepath.Abs(o.OutDir)
	if err != nil {
		return nil, err
	}
	absCfg, err := filepath.Abs(o.ContextDir)
	if err != nil {
		return nil, err
	}
	if _, err := os.Stat(filepath.Join(absCfg, "mkosi.conf")); err != nil {
		return nil, fmt.Errorf("no mkosi.conf in %s", absCfg)
	}
	// Mount the images/ directory rather than the single image, so a config can
	// reach a sibling: every base overlays ../common/tree. IMAGE picks which
	// subdirectory to build.
	imagesDir, imageName := filepath.Dir(absCfg), filepath.Base(absCfg)

	scripts, err := materialiseScripts()
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(scripts)

	args := []string{
		"run", "--rm",
		// mkosi creates user namespaces and mounts; this is the narrowest set
		// that works, rather than --privileged.
		"--cap-add", "SYS_ADMIN",
		"--security-opt", "seccomp=unconfined",
		"--security-opt", "apparmor=unconfined",
		"-e", "IMAGE=" + imageName,
		"-v", absOut + ":/out",
		"-v", imagesDir + ":/cfg:ro",
		"-v", scripts + ":/scripts:ro",
	}
	if o.Platform != "" {
		args = append(args, "--platform", o.Platform)
	}
	args = append(args, mkosiBuilderImage, "sh", "/scripts/mkosi.sh")

	if err := run(ctx, o.Verbose, "docker", args...); err != nil {
		return nil, fmt.Errorf("mkosi build: %w", err)
	}

	a := &Artifacts{Rootfs: filepath.Join(o.OutDir, "rootfs"), Tag: "mkosi:" + filepath.Base(absCfg)}
	if st, err := os.Stat(a.Rootfs); err != nil || !st.IsDir() {
		return nil, fmt.Errorf("expected artefact missing: %s", a.Rootfs)
	}
	return a, nil
}
