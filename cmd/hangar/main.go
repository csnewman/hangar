// Command hangar is the Hangar CLI.
//
// At this stage it does the minimum needed to prove the environment shape:
// build a guest kernel, build a VM image, and boot the two together.
package main

import (
	"context"
	"encoding/json"
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
	"github.com/csnewman/hangar/internal/qemu"
	"github.com/csnewman/hangar/internal/snapshot"
	"github.com/csnewman/hangar/internal/vm"
	"github.com/csnewman/hangar/internal/vmm"
	"github.com/csnewman/hangar/internal/vsock"
)

// defaultKernel is where `hangar kernel` leaves its build, and so where `hangar
// run` looks unless told otherwise.
const defaultKernel = "out/kernel/vmlinuz"

const usage = `hangar - development environments for coding agents

Usage:
  hangar doctor              check this machine can run environments
  hangar build [flags]       build a VM image with mkosi
  hangar kernel [flags]      build the guest kernel
  hangar vmm [flags]         build hangar-vmm, the KVM monitor
  hangar qemu   [flags]      build the QEMU that runs environments
  hangar pull  <ref>         pull an OCI image and unpack it for virtiofs
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
	case "vmm":
		err = buildVMM(ctx, os.Args[2:])
	case "kernel":
		err = buildKernel(ctx, os.Args[2:])
	case "qemu":
		err = buildQEMU(ctx, os.Args[2:])
	case "pull":
		err = pullImage(ctx, os.Args[2:])
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

func buildQEMU(ctx context.Context, argv []string) error {
	fs := flag.NewFlagSet("qemu", flag.ExitOnError)
	version := fs.String("version", qemu.DefaultVersion, "upstream QEMU version")
	arch := fs.String("arch", "", "target architecture (default: host)")
	out := fs.String("o", "out/qemu", "output directory")
	jobs := fs.Int("j", 0, "parallel build jobs (default: all cores)")
	verbose := fs.Bool("v", false, "show build output")
	if err := fs.Parse(argv); err != nil {
		return err
	}

	fmt.Fprintf(os.Stderr, "building qemu %s (this takes a while)\n", *version)
	a, err := qemu.Build(ctx, qemu.Options{
		Version: *version,
		Arch:    *arch,
		OutDir:  *out,
		Jobs:    *jobs,
		Verbose: *verbose,
	})
	if err != nil {
		return err
	}

	fmt.Fprintf(os.Stderr, "\nbuilt qemu %s\n", a.Version)
	if st, err := os.Stat(a.Binary); err == nil {
		fmt.Fprintf(os.Stderr, "  %-16s %s\n", filepath.Base(a.Binary), humanSize(st.Size()))
	}
	fmt.Fprintf(os.Stderr, "  %-16s %d\n", "devices", qemu.DeviceCount(a.Devices))
	return nil
}

// pullImage fetches an image and leaves it unpacked as a directory, which is
// the form an environment's read-only base layer is meant to take: virtiofs
// exports it, and environments sharing a base share its page cache.
func pullImage(ctx context.Context, argv []string) error {
	fs := flag.NewFlagSet("pull", flag.ExitOnError)
	root := fs.String("root", "out/images", "where content and snapshots live")
	platform := fs.String("platform", "", "platform to select (default: this host's)")
	key := fs.String("key", "", "name for the mounted view (default: derived from the ref)")
	mountIt := fs.Bool("mount", true, "mount the unpacked snapshot and print its path")
	if err := fs.Parse(argv); err != nil {
		return err
	}
	if fs.NArg() != 1 {
		return fmt.Errorf("usage: hangar pull [flags] <image-ref>")
	}
	ref := fs.Arg(0)

	st, err := snapshot.Open(*root)
	if err != nil {
		return err
	}
	defer st.Close()

	fmt.Fprintf(os.Stderr, "pulling %s\n", ref)
	chainID, err := st.Pull(ctx, ref, *platform)
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "chain       %s\n", chainID)

	if !*mountIt {
		return nil
	}
	k := *key
	if k == "" {
		k = strings.NewReplacer("/", "_", ":", "_").Replace(ref)
	}
	dir, err := st.Mount(ctx, chainID, k)
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "mounted     %s\n", dir)
	fmt.Println(dir)
	return nil
}

func runVM(ctx context.Context, argv []string) error {
	fs := flag.NewFlagSet("run", flag.ExitOnError)
	out := fs.String("o", "out", "directory holding the rootfs and kernel")
	mem := fs.Int("m", 4096, "memory in MiB")
	cpus := fs.Int("c", 2, "vCPUs")
	name := fs.String("name", "hangar-env", "environment name")
	printOnly := fs.Bool("print", false, "print the machine the monitor would be given, and exit")
	smoke := fs.Bool("smoke", false, "run the in-guest smoke test, then power off")
	console := fs.String("console", "", "write the guest console to this file instead of stdio")
	kernelPath := fs.String("kernel", "", "kernel to boot (default: "+defaultKernel+")")
	cid := fs.Uint("cid", 0, "guest vsock context ID (default: the first free one)")
	noAgent := fs.Bool("no-agent", false, "boot without an agent channel")
	agentWait := fs.Duration("agent-wait", 90*time.Second, "how long to wait for the agent")
	exec := fs.String("exec", "", "run this shell command in the guest once its agent answers, print the output, and stop")
	append_ := fs.String("append", "", "extra words for the guest kernel command line")
	execWait := fs.Duration("exec-timeout", 2*time.Minute, "how long to let -exec run")
	virtiofs := fs.String("virtiofs", "", "export this directory to the guest over virtiofs")
	monitor := fs.String("vmm", "qemu", "which monitor to run the guest under: qemu or hangar")
	network := fs.Bool("net", true, "give the guest outbound networking (qemu only)")
	dax := fs.Int("dax", 1024, "size of the virtiofs DAX window in MiB, for -vmm hangar (0 disables it)")
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

	// The base layer reaches the guest one of two ways, never both. Giving it
	// the same filesystem as a virtiofs export and as a block device is not
	// merely redundant: the host and guest would both mount one ext4, and the
	// guest hangs on it.
	disks := []vm.Disk{
		{Path: filepath.Join(*out, "upper.ext4")},
		{Path: filepath.Join(*out, "docker.ext4")},
	}
	if *virtiofs == "" {
		disks = append([]vm.Disk{
			{Path: filepath.Join(*out, "base.ext4"), ReadOnly: true},
		}, disks...)
	}

	cfg := &vm.Config{
		Name:        *name,
		Kernel:      kpath,
		Initrd:      filepath.Join(*out, "initrd.img"),
		Disks:       disks,
		MemoryMB:    *mem,
		Network:     *network,
		CPUs:        *cpus,
		ConsoleFile: *console,
	}

	if *smoke {
		cfg.ExtraCmdline = "hangar.smoketest"
	}
	if *append_ != "" {
		cfg.ExtraCmdline = strings.TrimSpace(cfg.ExtraCmdline + " " + *append_)
	}
	if !*noAgent {
		cfg.GuestCID = uint32(*cid)
		if cfg.GuestCID == 0 {
			cfg.GuestCID = vsock.SuggestGuestCID()
		}
	}

	var probe *probeCmd
	if *exec != "" {
		probe = &probeCmd{cmd: *exec, timeout: *execWait}
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

	// hangar-vmm serves the filesystem in its own process, so there is no
	// daemon to start and the export is part of the machine description.
	if *monitor == "hangar" {
		hcfg := &vmm.Config{
			Name:      cfg.Name,
			Kernel:    cfg.Kernel,
			Initrd:    cfg.Initrd,
			Cmdline:   "console=" + caps.ConsoleTTY + " systemd.show_status=1",
			MemoryMib: cfg.MemoryMB,
			CPUs:      cfg.CPUs,
			VsockCID:  cfg.GuestCID,
			Console:   cfg.ConsoleFile,
			Balloon:   &vmm.Balloon{FreePageReporting: true},
		}
		if *smoke {
			hcfg.Cmdline += " hangar.smoketest"
		}
		if *append_ != "" {
			hcfg.Cmdline += " " + *append_
		}
		for _, d := range cfg.Disks {
			hcfg.Disks = append(hcfg.Disks, vmm.Disk{Path: d.Path, ReadOnly: d.ReadOnly})
		}
		if *virtiofs != "" {
			hcfg.Fs = &vmm.Fs{
				SharedDir: *virtiofs,
				Tag:       "hangar-base",
				DaxMib:    *dax,
				Queues:    *cpus,
			}
		}
		if *printOnly {
			s, err := json.MarshalIndent(hcfg, "", "  ")
			if err != nil {
				return err
			}
			fmt.Println(string(s))
			return nil
		}
		return runUnder(ctx, hcfg, *agentWait, probe)
	}

	// virtiofsd has to be running before QEMU starts: QEMU connects to its
	// socket immediately and fails if nothing is listening.
	if *virtiofs != "" {
		sock := filepath.Join(os.TempDir(), "hangar-virtiofs-"+cfg.Name+".sock")
		vfs, err := vm.StartVirtiofsd(ctx, *virtiofs, sock, true)
		if err != nil {
			return err
		}
		defer vfs.Close()
		cfg.VirtiofsSocket = vfs.Socket()
		fmt.Fprintf(os.Stderr, "virtiofs    %s -> tag hangar-base\n", *virtiofs)
	}

	if *printOnly {
		s, err := vm.PrintCommand(caps, cfg)
		if err != nil {
			return err
		}
		fmt.Println(s)
		return nil
	}

	fmt.Fprintf(os.Stderr, "booting %s (%s, %s, %d MiB, %d vCPU)\n",
		cfg.Name, caps.Machine, caps.Accel, cfg.MemoryMB, cfg.CPUs)
	if cfg.ConsoleFile != "" {
		fmt.Fprintf(os.Stderr, "console -> %s\n\n", cfg.ConsoleFile)
	} else {
		fmt.Fprintf(os.Stderr, "console follows; quit with Ctrl-A then X\n\n")
	}

	return withAgent(ctx, cfg.GuestCID, *agentWait, probe, func() error {
		return vm.Run(ctx, caps, cfg)
	})
}

// runUnder boots a guest under hangar-vmm, waiting for its agent the same way
// the QEMU path does.
func runUnder(ctx context.Context, cfg *vmm.Config, agentWait time.Duration, probe *probeCmd) error {
	fmt.Fprintf(os.Stderr, "booting %s under hangar-vmm (%d MiB, %d vCPU)\n",
		cfg.Name, cfg.MemoryMib, cfg.CPUs)
	if cfg.Fs != nil {
		fmt.Fprintf(os.Stderr, "virtiofs    %s -> tag %s, dax %d MiB\n",
			cfg.Fs.SharedDir, cfg.Fs.Tag, cfg.Fs.DaxMib)
	}
	return withAgent(ctx, cfg.VsockCID, agentWait, probe, func() error {
		return vmm.Run(ctx, cfg)
	})
}

// withAgent boots a guest and proves its agent channel works.
//
// The listener is opened before the guest starts. The agent dials out early
// in the boot, so a listener opened afterwards would miss its first attempts
// and only succeed once it retried.
func withAgent(ctx context.Context, cid uint32, agentWait time.Duration, probe *probeCmd, boot func() error) error {
	if cid == 0 {
		return boot()
	}

	srv, err := agent.Listen()
	if err != nil {
		return fmt.Errorf("starting the agent channel: %w", err)
	}
	defer srv.Close()

	vmDone := make(chan error, 1)
	go func() { vmDone <- boot() }()

	sess, err := srv.Accept(agentWait)
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

	// A command to run inside the environment is a diagnostic: it reports
	// what the guest sees rather than what the console shows, which is the
	// only way to ask systemd about its own startup.
	if probe != nil {
		out, err := sess.Exec(probe.timeout, "sh", "-c", probe.cmd)
		if err != nil {
			return err
		}
		fmt.Print(out.Stdout)
		if out.Stderr != "" {
			fmt.Fprint(os.Stderr, out.Stderr)
		}
		return nil
	}

	return <-vmDone
}

// probeCmd is a command to run in the guest once it answers.
type probeCmd struct {
	cmd     string
	timeout time.Duration
}

func buildVMM(ctx context.Context, argv []string) error {
	fs := flag.NewFlagSet("vmm", flag.ExitOnError)
	debug := fs.Bool("debug", false, "build without optimisation, for faster iteration")
	if err := fs.Parse(argv); err != nil {
		return err
	}
	path, err := vmm.Build(ctx, !*debug)
	if err != nil {
		return err
	}
	info, err := os.Stat(path)
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "\nbuilt %s (%s)\n", path, humanSize(info.Size()))
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
