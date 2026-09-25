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

	"github.com/csnewman/hangar/internal/ch"
	"github.com/csnewman/hangar/internal/host"
	"github.com/csnewman/hangar/internal/image"
	"github.com/csnewman/hangar/internal/kernel"
	"github.com/csnewman/hangar/internal/snapshot"
	"github.com/csnewman/hangar/internal/vm"
)

// defaultKernel is where `hangar kernel` leaves its build, and so where `hangar
// run` looks unless told otherwise.
const defaultKernel = "out/kernel/vmlinuz"

const usage = `hangar - development environments for coding agents

Usage:
  hangar doctor              check this machine can run environments
  hangar build [flags]       build a VM image with mkosi
  hangar kernel [flags]      build the guest kernel
  hangar pull  <ref>         pull an OCI image and unpack it for virtiofs
  hangar run   [flags]       boot an environment and attach to its console
  hangar exec    -name N -- cmd   run a command in a running environment
  hangar suspend -name N     write a running environment to disk and stop it
  hangar resume  -name N     bring a suspended environment back, even after a host restart

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
	case "pull":
		err = pullImage(ctx, os.Args[2:])
	case "resume":
		err = resumeVM(ctx, os.Args[2:])
	case "suspend":
		err = suspendEnv(ctx, os.Args[2:])
	case "exec":
		err = execEnv(os.Args[2:])
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

func build(ctx context.Context, argv []string) error {
	fs := flag.NewFlagSet("build", flag.ExitOnError)
	dir := fs.String("C", "images/ubuntu2604", "image directory containing mkosi.conf")
	out := fs.String("o", "out", "output directory, on a Linux filesystem; the image is <o>/rootfs")
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

	fmt.Fprintf(os.Stderr, "\nbuilt %s\n", a.Rootfs)
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
	out := fs.String("o", "out", "directory holding the image (rootfs/) and the environment's disks")
	mem := fs.Int("m", 4096, "memory in MiB")
	cpus := fs.Int("c", 2, "vCPUs")
	name := fs.String("name", "hangar-env", "environment name")
	printOnly := fs.Bool("print", false, "print the machine the monitor would be given, and exit")
	smoke := fs.Bool("smoke", false, "run the in-guest smoke test, then power off")
	console := fs.String("console", "", "write the guest console to this file instead of stdio")
	kernelPath := fs.String("kernel", "", "kernel to boot (default: "+defaultKernel+")")
	agentWait := fs.Duration("agent-wait", 90*time.Second, "how long to wait for the agent")
	exec := fs.String("exec", "", "run this shell command in the guest once its agent answers, print the output, and stop")
	append_ := fs.String("append", "", "extra words for the guest kernel command line")
	execWait := fs.Duration("exec-timeout", 2*time.Minute, "how long to let -exec run")
	baseDir := fs.String("base", "", "the image's root filesystem (default: <o>/rootfs)")
	agentPath := fs.String("agent", "", "hangar-agent for the guest, its init (default: one built for the guest)")
	upperGiB := fs.Int("upper", 16, "size of a new writable layer in GiB")
	dockerGiB := fs.Int("docker", 24, "size of a new Docker disk in GiB")
	dax := fs.Int("dax", 1024, "MiB of DAX window for the virtiofs root; 0 turns mapping off")
	network := fs.Bool("net", true, "give the guest outbound networking")
	gpu := fs.Bool("gpu", false, "give the guest a virtio-gpu device")
	gpuVenus := fs.Bool("gpu-venus", false, "offer Vulkan through venus, with -gpu")
	gpuVenusRestore := fs.Bool("gpu-venus-restore", false, "carry Vulkan state across a suspend (experimental); without it a Vulkan program ends on resume")
	gpuShm := fs.Int("gpu-window", 512, "MiB the guest may map blob resources into, with -gpu")
	seccomp := fs.String("seccomp", "", "monitor syscall filtering: true, false, log or errno")
	if err := fs.Parse(argv); err != nil {
		return err
	}

	// The base goes over virtio-fs, vda is the writable upper layer, and the
	// agent -- the initramfs, as init -- stacks them with overlayfs and hands
	// over to the image's init.
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
	base := *baseDir
	if base == "" {
		base = filepath.Join(*out, "rootfs")
	}
	if st, err := os.Stat(base); err != nil || !st.IsDir() {
		return fmt.Errorf("no image at %s - run \"hangar build -o %s\" first", base, *out)
	}
	disks := []ch.Disk{
		{Path: filepath.Join(*out, "upper.ext4")},
		{Path: filepath.Join(*out, "docker.ext4")},
	}
	if err := vm.EnsureDisk(ctx, disks[0].Path, "hangar-upper", *upperGiB); err != nil {
		return err
	}
	if err := vm.EnsureDisk(ctx, disks[1].Path, "hangar-docker", *dockerGiB); err != nil {
		return err
	}

	dir, err := stateDir(*name)
	if err != nil {
		return err
	}
	cfg := vm.InstanceConfig{
		ID: *name, Name: *name, Dir: dir,
		Kernel: kpath, Base: base, Disks: disks,
		MemoryMiB: *mem, CPUs: *cpus, DaxMiB: *dax, Net: *network,
		GPU: *gpu, GPUVenus: *gpuVenus, GPUVenusRestore: *gpuVenusRestore, GPUWindowMiB: *gpuShm,
		ConsoleFile: *console, Seccomp: *seccomp,
	}
	if *smoke {
		cfg.ExtraCmdline = "hangar.smoketest"
	}
	if *append_ != "" {
		cfg.ExtraCmdline = strings.TrimSpace(cfg.ExtraCmdline + " " + *append_)
	}
	if *printOnly {
		out, err := cfg.Command()
		if err != nil {
			return err
		}
		fmt.Println(out)
		return nil
	}

	cfg.Agent = *agentPath
	if cfg.Agent == "" {
		built, err := image.BuildAgent(ctx, "linux/"+runtime.GOARCH, false)
		if err != nil {
			return fmt.Errorf("building the agent: %w", err)
		}
		// Kept with the machine: a resume boots with the same agent.
		cfg.Agent = filepath.Join(dir, "hangar-agent")
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return err
		}
		if err := os.Rename(filepath.Join(built, "hangar-agent"), cfg.Agent); err != nil {
			return err
		}
		os.RemoveAll(built)
	}

	// Guest memory has to be shared for virtio-fs, and shared memory gets
	// huge pages only if the host allows it. Nothing fails without them, the
	// guest is merely three to five times slower, so this is checked rather
	// than left to be discovered.
	if err := ch.CheckHugePages(); err != nil {
		return err
	}
	// A run boots afresh: a snapshot left from an earlier suspend is resumed
	// by "hangar resume", not here.
	vm.DiscardSuspend(dir)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	if err := cfg.Save(); err != nil {
		return err
	}

	var probe *probeCmd
	if *exec != "" {
		probe = &probeCmd{cmd: *exec, timeout: *execWait}
	}
	fmt.Fprintf(os.Stderr, "booting %s under cloud-hypervisor (%d MiB, %d vCPU)\n", cfg.Name, cfg.MemoryMiB, cfg.CPUs)
	if cfg.ConsoleFile != "" {
		fmt.Fprintf(os.Stderr, "console -> %s\n\n", cfg.ConsoleFile)
	}
	return holdMachine(ctx, cfg, *agentWait, probe)
}

// resumeVM brings a suspended environment back.
func resumeVM(ctx context.Context, argv []string) error {
	fs := flag.NewFlagSet("resume", flag.ExitOnError)
	name := fs.String("name", "hangar-env", "environment name")
	agentWait := fs.Duration("agent-wait", 90*time.Second, "how long to wait for the agent to reconnect")
	exec := fs.String("exec", "", "run this shell command in the guest once its agent answers, print the output, and stop")
	execWait := fs.Duration("exec-timeout", 2*time.Minute, "how long to let -exec run")
	if err := fs.Parse(argv); err != nil {
		return err
	}
	dir, err := stateDir(*name)
	if err != nil {
		return err
	}
	if !vm.Suspended(dir) {
		return fmt.Errorf("%s is not suspended: %s holds no snapshot of it", *name, dir)
	}
	cfg, err := vm.LoadInstanceConfig(dir)
	if err != nil {
		return fmt.Errorf("%s has no saved machine: %w", *name, err)
	}
	if err := ch.CheckHugePages(); err != nil {
		return err
	}
	var probe *probeCmd
	if *exec != "" {
		probe = &probeCmd{cmd: *exec, timeout: *execWait}
	}
	fmt.Fprintf(os.Stderr, "resuming %s from %s\n", cfg.Name, dir)
	return holdMachine(ctx, cfg, *agentWait, probe)
}

// probeCmd is a command to run in the guest once it answers.
type probeCmd struct {
	cmd     string
	timeout time.Duration
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
