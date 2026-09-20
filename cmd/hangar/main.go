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
	"strings"
	"time"

	"github.com/csnewman/hangar/internal/agent"
	"github.com/csnewman/hangar/internal/host"
	"github.com/csnewman/hangar/internal/image"
	"github.com/csnewman/hangar/internal/kernel"
	"github.com/csnewman/hangar/internal/vm"
	"github.com/csnewman/hangar/internal/vsock"
)

// defaultKernel is where `hangar kernel` leaves its build, and so where `hangar
// run` looks unless told otherwise.
const defaultKernel = "out/kernel/vmlinuz"

const usage = `hangar - development environments for coding agents

Usage:
  hangar doctor              check this machine can run environments
  hangar build [flags]       build a VM image with mkosi
  hangar kernel [flags]      build the guest kernel and its modules
  hangar run   [flags]       boot an environment and attach to its console

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
	case "kernel":
		err = buildKernel(ctx, os.Args[2:])
	case "run":
		err = runVM(ctx, os.Args[2:])
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

	if caps.Accel == host.AccelNone {
		fmt.Println()
		fmt.Println("No hardware acceleration. Environments will boot, but slowly.")
	}
	return nil
}

func build(ctx context.Context, argv []string) error {
	fs := flag.NewFlagSet("build", flag.ExitOnError)
	dir := fs.String("C", "images/ubuntu2604", "image directory containing mkosi.conf")
	out := fs.String("o", "out", "output directory for the rootfs")
	size := fs.Int("size", 8, "base image size in GiB")
	upper := fs.Int("upper", 16, "writable layer size in GiB")
	dockerSz := fs.Int("docker", 24, "docker image store size in GiB")
	verbose := fs.Bool("v", false, "show build output")
	if err := fs.Parse(argv); err != nil {
		return err
	}

	// The guest architecture follows the host, so the image must match.
	platform := "linux/" + runtime.GOARCH

	opts := image.Options{
		ContextDir: *dir,
		OutDir:     *out,
		SizeGB:     *size,
		UpperGB:    *upper,
		DockerGB:   *dockerSz,
		Platform:   platform,
		Verbose:    *verbose,
	}

	fmt.Fprintf(os.Stderr, "building %s with mkosi for %s\n", *dir, platform)
	a, err := image.BuildWithMkosi(ctx, opts)
	if err != nil {
		return err
	}

	fmt.Fprintln(os.Stderr, "\nbuilt:")
	for _, p := range []string{a.Initrd, a.Base, a.Upper, a.Docker} {
		if st, err := os.Stat(p); err == nil {
			fmt.Fprintf(os.Stderr, "  %-24s %s\n", filepath.Base(p), humanSize(st.Size()))
		}
	}
	fmt.Fprintf(os.Stderr, "\nboot it with:  hangar run -o %s\n", *out)
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

func runVM(ctx context.Context, argv []string) error {
	fs := flag.NewFlagSet("run", flag.ExitOnError)
	out := fs.String("o", "out", "directory holding the rootfs and kernel")
	mem := fs.Int("m", 4096, "memory in MiB")
	cpus := fs.Int("c", 2, "vCPUs")
	name := fs.String("name", "hangar-env", "environment name")
	printOnly := fs.Bool("print", false, "print the QEMU command line and exit")
	smoke := fs.Bool("smoke", false, "run the in-guest smoke test, then power off")
	console := fs.String("console", "", "write the guest console to this file instead of stdio")
	kernelPath := fs.String("kernel", "", "kernel to boot (default: "+defaultKernel+")")
	cid := fs.Uint("cid", 0, "guest vsock context ID (default: the first free one)")
	noAgent := fs.Bool("no-agent", false, "boot without an agent channel")
	agentWait := fs.Duration("agent-wait", 90*time.Second, "how long to wait for the agent")
	if err := fs.Parse(argv); err != nil {
		return err
	}

	caps, err := host.Detect()
	if err != nil {
		return err
	}

	// vda is the read-only base, vdb the writable upper layer. The initramfs
	// stacks them with overlayfs and switch_roots into the result.
	//
	// The kernel is Hangar's own and comes from `hangar kernel`, never from the
	// image: images are root filesystems, and one kernel serves all of them.
	kpath := *kernelPath
	if kpath == "" {
		kpath = defaultKernel
		if _, err := os.Stat(kpath); err != nil {
			return fmt.Errorf("no guest kernel at %s - run \"hangar kernel\" first, or pass -kernel", kpath)
		}
	}

	cfg := &vm.Config{
		Name:   *name,
		Kernel: kpath,
		Initrd: filepath.Join(*out, "initrd.img"),
		Disks: []vm.Disk{
			{Path: filepath.Join(*out, "base.ext4"), ReadOnly: true},
			{Path: filepath.Join(*out, "upper.ext4")},
			{Path: filepath.Join(*out, "docker.ext4")},
		},
		MemoryMB:    *mem,
		Network:     true,
		CPUs:        *cpus,
		ConsoleFile: *console,
	}

	if *smoke {
		cfg.ExtraCmdline = "hangar.smoketest"
	}
	if !*noAgent {
		cfg.GuestCID = uint32(*cid)
		if cfg.GuestCID == 0 {
			cfg.GuestCID = vsock.SuggestGuestCID()
		}
	}

	if *printOnly {
		s, err := vm.PrintCommand(caps, cfg)
		if err != nil {
			return err
		}
		fmt.Println(s)
		return nil
	}

	required := []string{cfg.Kernel, cfg.Initrd}
	for _, d := range cfg.Disks {
		required = append(required, d.Path)
	}
	for _, p := range required {
		if _, err := os.Stat(p); err != nil {
			return fmt.Errorf("%s not found - run \"hangar build\" first", p)
		}
	}

	fmt.Fprintf(os.Stderr, "booting %s (%s, %s, %d MiB, %d vCPU)\n",
		cfg.Name, caps.Machine, caps.Accel, cfg.MemoryMB, cfg.CPUs)
	if cfg.ConsoleFile != "" {
		fmt.Fprintf(os.Stderr, "console -> %s\n\n", cfg.ConsoleFile)
	} else {
		fmt.Fprintf(os.Stderr, "console follows; quit with Ctrl-A then X\n\n")
	}

	if cfg.GuestCID == 0 {
		return vm.Run(ctx, caps, cfg)
	}

	// Listen before booting. The agent starts early in the guest, so a
	// listener opened afterwards would miss the first connection attempts and
	// only succeed on a retry.
	srv, err := agent.Listen()
	if err != nil {
		return fmt.Errorf("starting the agent channel: %w", err)
	}
	defer srv.Close()

	vmDone := make(chan error, 1)
	go func() { vmDone <- vm.Run(ctx, caps, cfg) }()

	sess, err := srv.Accept(*agentWait)
	if err != nil {
		// A VM that died explains the missing agent better than a timeout
		// does, so prefer that error if one is waiting.
		select {
		case verr := <-vmDone:
			if verr != nil {
				return verr
			}
			return fmt.Errorf("the environment exited before its agent connected: %w", err)
		default:
		}
		return err
	}
	defer sess.Close()

	fmt.Fprintf(os.Stderr, "\nagent       cid %d, %s, kernel %s, ready in %.2fs\n",
		sess.CID, sess.Hello.Hostname, sess.Hello.Kernel,
		float64(sess.Hello.BootMicros)/1e6)

	// Prove the channel end to end rather than just that something connected:
	// a ping exercises the request path, and running a command exercises the
	// half an environment is actually for.
	if err := sess.Ping(5 * time.Second); err != nil {
		return fmt.Errorf("agent did not answer a ping: %w", err)
	}
	who, err := sess.Exec(10*time.Second, "id", "-un")
	if err != nil {
		return fmt.Errorf("agent could not run a command: %w", err)
	}
	fmt.Fprintf(os.Stderr, "agent       ping ok, exec ok (runs as %s)\n",
		strings.TrimSpace(who.Stdout))

	return <-vmDone
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
