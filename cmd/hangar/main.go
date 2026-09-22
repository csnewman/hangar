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
	"github.com/csnewman/hangar/internal/ch"
	"github.com/csnewman/hangar/internal/host"
	"github.com/csnewman/hangar/internal/image"
	"github.com/csnewman/hangar/internal/kernel"
	"github.com/csnewman/hangar/internal/snapshot"
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
	case "kernel":
		err = buildKernel(ctx, os.Args[2:])
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
	network := fs.Bool("net", true, "give the guest outbound networking")
	gpu := fs.Bool("gpu", false, "give the guest a virtio-gpu device")
	gpuVenus := fs.Bool("gpu-venus", false, "offer Vulkan through venus, with -gpu")
	seccomp := fs.String("seccomp", "", "monitor syscall filtering: true, false, log or errno")
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
	disks := []ch.Disk{
		{Path: filepath.Join(*out, "upper.ext4")},
		{Path: filepath.Join(*out, "docker.ext4")},
	}
	if *virtiofs == "" {
		disks = append([]ch.Disk{
			{Path: filepath.Join(*out, "base.ext4"), ReadOnly: true},
		}, disks...)
	}

	ccfg := &ch.Config{
		Name:        *name,
		Kernel:      kpath,
		Initrd:      filepath.Join(*out, "initrd.img"),
		Disks:       disks,
		MemoryMB:    *mem,
		CPUs:        *cpus,
		ConsoleFile: *console,
		ConsoleTTY:  caps.ConsoleTTY,
		GPU:         *gpu,
		GPUVenus:    *gpuVenus,
		Seccomp:     *seccomp,
	}

	if *smoke {
		ccfg.ExtraCmdline = "hangar.smoketest"
	}
	if *append_ != "" {
		ccfg.ExtraCmdline = strings.TrimSpace(ccfg.ExtraCmdline + " " + *append_)
	}
	if !*noAgent {
		ccfg.GuestCID = uint32(*cid)
		if ccfg.GuestCID == 0 {
			ccfg.GuestCID = vsock.SuggestGuestCID()
		}
	}

	var probe *probeCmd
	if *exec != "" {
		probe = &probeCmd{cmd: *exec, timeout: *execWait}
	}

	required := []string{ccfg.Kernel, ccfg.Initrd}
	for _, d := range ccfg.Disks {
		required = append(required, d.Path)
	}
	for _, p := range required {
		if _, err := os.Stat(p); err != nil {
			return fmt.Errorf("%s not found - run \"hangar build\" first", p)
		}
	}

	// Guest memory has to be shared for virtio-fs, and shared memory gets
	// huge pages only if the host allows it. Nothing fails without them, the
	// guest is merely three to five times slower, so this is checked rather
	// than left to be discovered.
	if err := ch.CheckHugePages(); err != nil {
		return err
	}

	run := filepath.Join(os.TempDir(), "hangar-"+ccfg.Name)
	if ccfg.GuestCID != 0 {
		ccfg.VsockSocket = run + "-vsock.sock"
	}

	if *printOnly {
		if *virtiofs != "" {
			ccfg.VirtiofsSocket = run + "-virtiofs.sock"
		}
		if *network {
			ccfg.NetSocket = run + "-net.sock"
		}
		out, err := ch.PrintCommand(ccfg)
		if err != nil {
			return err
		}
		fmt.Println(out)
		return nil
	}

	// Both backends own their sockets and must be listening before the
	// monitor starts: it connects to them as a client and gives up if
	// nothing is there.
	if *virtiofs != "" {
		vfs, err := ch.StartVirtiofsd(ctx, *virtiofs, run+"-virtiofs.sock", true)
		if err != nil {
			return err
		}
		defer vfs.Close()
		ccfg.VirtiofsSocket = vfs.Socket()
		fmt.Fprintf(os.Stderr, "virtiofs    %s -> tag hangar-base\n", *virtiofs)
	}
	if *network {
		pst, err := ch.StartPasst(ctx, run+"-net.sock", false)
		if err != nil {
			return err
		}
		defer pst.Close()
		ccfg.NetSocket = pst.Socket()
	}

	// Cloud Hypervisor carries vsock over a unix socket rather than the
	// host kernel, so the agent is waited for on that socket instead of
	// on AF_VSOCK.
	var srv *agent.Server
	if ccfg.GuestCID != 0 {
		// The monitor binds this path itself, so a socket left by one
		// that was killed rather than stopped would keep it from
		// starting. The agent's own socket is a different path and is
		// cleaned up by the listener.
		_ = os.Remove(ccfg.VsockSocket)
		srv, err = agent.ListenHybrid(ccfg.VsockSocket, ccfg.GuestCID)
		if err != nil {
			return fmt.Errorf("starting the agent channel: %w", err)
		}
	}

	fmt.Fprintf(os.Stderr, "booting %s under cloud-hypervisor (%d MiB, %d vCPU)\n",
		ccfg.Name, ccfg.MemoryMB, ccfg.CPUs)
	if ccfg.ConsoleFile != "" {
		fmt.Fprintf(os.Stderr, "console -> %s\n\n", ccfg.ConsoleFile)
	}

	// The monitor has to go before its backends do. -exec returns as soon
	// as the command has run, with the guest still up, and tearing
	// virtiofsd or passt out from under a live vhost-user connection makes
	// the monitor report a broken device on the way out. Deferred calls
	// run last-registered first, so this one precedes both Closes above.
	vmCtx, stopVM := context.WithCancel(ctx)
	vmDone := make(chan struct{})
	defer func() {
		stopVM()
		<-vmDone
	}()

	return withAgent(ctx, srv, *agentWait, probe, func() error {
		defer close(vmDone)
		return ch.Run(vmCtx, ccfg)
	})
}

// withAgent boots a guest and proves its agent channel works.
//
// The listener is opened by the caller, before the guest starts. The agent
// dials out early in the boot, so a listener opened afterwards would miss its
// first attempts and only succeed once it retried.
//
// A nil server means the guest has no agent channel.
func withAgent(ctx context.Context, srv *agent.Server, agentWait time.Duration, probe *probeCmd, boot func() error) error {
	if srv == nil {
		return boot()
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
