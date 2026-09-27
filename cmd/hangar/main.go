// Command hangar is the Hangar CLI.
//
// At this stage it does the minimum needed to prove the environment shape:
// build a guest kernel, build a VM image, and boot the two together.
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"time"

	"github.com/csnewman/hangar/internal/ch"
	"github.com/csnewman/hangar/internal/host"
	"github.com/csnewman/hangar/internal/image"
	"github.com/csnewman/hangar/internal/kernel"
)

const usage = `hangar - development environments for coding agents

Usage:
  hangar doctor              check this machine can run environments
  hangar build [flags]       build a distribution's images with mkosi
  hangar oci   [flags]       write built images as an OCI layout, to push
  hangar kernel [flags]      build the guest kernel

Run "hangar <command> -h" for the flags of a command.
`

func main() {
	if len(os.Args) < 2 {
		fmt.Fprint(os.Stderr, usage)
		os.Exit(2)
	}

	ctx := context.Background()
	var err error

	switch os.Args[1] {
	case "doctor":
		err = doctor()
	case "build":
		err = build(ctx, os.Args[2:])
	case "oci":
		err = writeOCI(ctx, os.Args[2:])
	case "kernel":
		err = buildKernel(ctx, os.Args[2:])
	case "-h", "--help", "help":
		fmt.Print(usage)
		return
	default:
		fmt.Fprintf(os.Stderr, "unknown command %q\n\n%s", os.Args[1], usage)
		os.Exit(2)
	}

	if err != nil {
		fmt.Fprintf(os.Stderr, "hangar: %v\n", err)
		os.Exit(1)
	}
}

func doctor() error {
	caps, err := host.Detect()
	if err != nil {
		fmt.Printf("host        %s/%s\n", runtime.GOOS, runtime.GOARCH)
		return err
	}
	fmt.Print(caps.Summary())

	// Cloud Hypervisor runs the environments, so its state belongs here.
	if bin, err := ch.Find(); err != nil {
		fmt.Printf("cloud-hyp   not available: %v\n", err)
	} else {
		fmt.Printf("cloud-hyp   %s\n", bin)
	}
	if err := ch.CheckHugePages(); err != nil {
		fmt.Printf("huge pages  NO\n")
	} else {
		fmt.Printf("huge pages  yes, shared memory is eligible\n")
	}

	// The full explanation is long and only worth printing when it applies.
	if err := ch.CheckHugePages(); err != nil {
		fmt.Println()
		fmt.Println(err)
	}
	return nil
}

func writeOCI(ctx context.Context, argv []string) error {
	fs := flag.NewFlagSet("oci", flag.ExitOnError)
	in := fs.String("i", "out", "what `hangar build -o` wrote: <i>/<tier>/rootfs")
	out := fs.String("o", "out/oci", "OCI layout to write, with an image per tier named by its tier")
	arch := fs.String("arch", runtime.GOARCH, "the images' architecture")
	if err := fs.Parse(argv); err != nil {
		return err
	}
	rootfs := map[string]string{}
	for _, tier := range image.Tiers {
		rootfs[tier] = filepath.Join(*in, tier, "rootfs")
	}
	if err := image.WriteOCI(ctx, *out, image.OCIOptions{Rootfs: rootfs, Arch: *arch, Created: time.Now().UTC()}); err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "wrote %s: %v, each named by its tier\n", *out, image.Tiers)
	return nil
}

func build(ctx context.Context, argv []string) error {
	fs := flag.NewFlagSet("build", flag.ExitOnError)
	dir := fs.String("C", "images/ubuntu-26.04", "distribution directory, holding minimal/ and base/")
	out := fs.String("o", "out", "output directory, on a Linux filesystem; each tier's image is <o>/<tier>/rootfs")
	verbose := fs.Bool("v", false, "show build output")
	if err := fs.Parse(argv); err != nil {
		return err
	}

	// The guest architecture follows the host, so the image must match.
	platform := "linux/" + runtime.GOARCH

	opts := image.Options{
		ContextDir: *dir,
		OutDir:     *out,
		Platform:   platform,
		Verbose:    *verbose,
	}

	fmt.Fprintf(os.Stderr, "building %s with mkosi for %s\n", *dir, platform)
	a, err := image.BuildWithMkosi(ctx, opts)
	if err != nil {
		return err
	}

	fmt.Fprintln(os.Stderr)
	for _, tier := range image.Tiers {
		fmt.Fprintf(os.Stderr, "built %s\n", a.Rootfs[tier])
	}
	fmt.Fprintf(os.Stderr, "build %s\n", a.Build)
	return nil
}

func buildKernel(ctx context.Context, argv []string) error {
	fs := flag.NewFlagSet("kernel", flag.ExitOnError)
	version := fs.String("version", kernel.DefaultVersion, "upstream kernel version")
	arch := fs.String("arch", "", "guest architecture (default: host)")
	out := fs.String("o", "out/kernel", "output directory")
	jobs := fs.Int("j", 0, "parallel build jobs (default: all cores)")
	verbose := fs.Bool("v", false, "show build output")
	base := fs.String("base", "tinyconfig", "kconfig base target the fragment merges onto")
	configOnly := fs.Bool("config-only", false, "resolve and verify the config without compiling")
	if err := fs.Parse(argv); err != nil {
		return err
	}

	if *configOnly {
		fmt.Fprintf(os.Stderr, "resolving kernel config %s (base=%s)\n", *version, *base)
	} else {
		fmt.Fprintf(os.Stderr, "building kernel %s (this takes a while)\n", *version)
	}
	a, err := kernel.Build(ctx, kernel.Options{
		Version:    *version,
		Arch:       *arch,
		OutDir:     *out,
		Jobs:       *jobs,
		Verbose:    *verbose,
		Base:       *base,
		ConfigOnly: *configOnly,
	})
	if err != nil {
		return err
	}

	if *configOnly {
		fmt.Fprintf(os.Stderr, "config written to %s\n", a.Config)
		return nil
	}
	fmt.Fprintf(os.Stderr, "\nbuilt %s\n", a.KernelRelease)
	if st, err := os.Stat(a.Image); err == nil {
		fmt.Fprintf(os.Stderr, "  %-16s %s\n", "vmlinuz", humanSize(st.Size()))
	}
	fmt.Fprintf(os.Stderr, "  %-16s %s\n", "config", a.Config)
	return nil
}

func humanSize(n int64) string {
	const unit = 1024
	if n < unit {
		return fmt.Sprintf("%d B", n)
	}
	div, exp := int64(unit), 0
	for m := n / unit; m >= unit; m /= unit {
		div *= unit
		exp++
	}
	return fmt.Sprintf("%.1f %ciB", float64(n)/float64(div), "KMGT"[exp])
}
