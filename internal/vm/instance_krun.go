package vm

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"syscall"
	"time"

	"github.com/csnewman/hangar/internal/agent"
	"github.com/csnewman/hangar/internal/ch"
	"github.com/csnewman/hangar/internal/code"
	"github.com/csnewman/hangar/internal/desktop"
	"github.com/csnewman/hangar/internal/guestport"
	"github.com/csnewman/hangar/internal/host"
	"github.com/csnewman/hangar/internal/krun"
	"github.com/csnewman/hangar/internal/procs"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/sshd"
	"github.com/csnewman/hangar/internal/terminal"
	"github.com/csnewman/hangar/internal/vscode"
	"github.com/csnewman/hangar/internal/vsock"
)

// guestPorts are the vsock ports the worker reaches a guest's services on.
// libkrun carries each on a unix socket of its own, which it listens on.
var guestPorts = []uint32{terminal.Port, vscode.Port, guestport.Port, sshd.Port, procs.Port, profile.Port,
	code.Port, desktop.Port}

// errNoKrunSnapshot is what libkrun cannot do that Cloud Hypervisor can.
var errNoKrunSnapshot = fmt.Errorf("suspending a machine under libkrun is %w: it takes no snapshot", errUnsupported)

// krunDir is where a machine under libkrun has its vsock sockets: under
// /tmp, since a unix socket's path is at most 103 bytes on macOS, which a
// worker's state directory leaves too little of.
func krunDir(id string) (string, error) {
	dir := filepath.Join("/tmp", "hangar-"+id)
	return dir, os.MkdirAll(dir, 0o700)
}

// guestSocket is where libkrun listens for connections to the guest's port.
func guestSocket(base string, port uint32) string {
	return fmt.Sprintf("%s-g%d", base, port)
}

// startKrun starts the machine under hangar-krun -- libkrun -- and listens
// for its agent. libkrun takes no snapshots, so a machine is always booted
// from its disks.
func (i *Instance) startKrun(ctx, procCtx, monCtx context.Context, initrd string, caps *host.Caps,
	resuming bool, progress func(string)) error {
	cfg := i.cfg
	if resuming {
		return errNoKrunSnapshot
	}
	dir, err := krunDir(cfg.ID)
	if err != nil {
		return err
	}
	i.backends = append(i.backends, closer(func() { os.RemoveAll(dir) }))
	i.vsock = filepath.Join(dir, "v")

	kc := krun.Config{
		CPUs:      cfg.CPUs,
		MemoryMiB: cfg.MemoryMiB,
		Kernel:    cfg.Kernel,
		Initrd:    initrd,
		Console:   cfg.ConsoleFile,
		HostPorts: map[uint32]string{agent.Port: vsock.HybridPath(i.vsock, agent.Port)},
	}
	for n, d := range cfg.Disks {
		kc.Disks = append(kc.Disks, krun.Disk{ID: fmt.Sprintf("vd%c", 'a'+n), Path: d.Path, ReadOnly: d.ReadOnly,
			Direct: d.Direct})
	}
	extra := cfg.ExtraCmdline
	switch cfg.BaseDevice {
	case ImageDisk:
		// After the disks the guest already knows by position and label.
		dev := fmt.Sprintf("vd%c", 'a'+len(cfg.Disks))
		kc.Disks = append(kc.Disks, krun.Disk{ID: dev, Path: cfg.BaseFile, ReadOnly: true})
		extra += " hangar.image=/dev/" + dev
	default:
		return fmt.Errorf("libkrun attaches an image as a disk only (vm.image_device %s), not %q", ImageDisk, cfg.BaseDevice)
	}
	kc.Cmdline = (&ch.Config{ConsoleTTY: caps.ConsoleTTY, ExtraCmdline: extra}).Cmdline()
	kc.GuestPorts = map[uint32]string{}
	for _, p := range guestPorts {
		kc.GuestPorts[p] = guestSocket(i.vsock, p)
	}
	if cfg.Net && caps.OS == "linux" {
		// passt, as under Cloud Hypervisor, speaking its stream protocol.
		sock := filepath.Join(dir, "net")
		p, err := ch.StartPasst(procCtx, sock, false)
		if err != nil {
			return fmt.Errorf("starting the network: %w", err)
		}
		i.backends = append(i.backends, p)
		kc.Net = &krun.Net{Socket: sock, MAC: guestMAC, Stream: true}
	} else if cfg.Net {
		sock := filepath.Join(dir, "net")
		n, err := startNetwork(procCtx, sock)
		if err != nil {
			return fmt.Errorf("starting the network: %w", err)
		}
		i.backends = append(i.backends, n)
		kc.Net = &krun.Net{Socket: sock, MAC: guestMAC}
	}
	if cfg.GPU {
		window := cfg.GPUWindowMiB
		if window == 0 {
			window = 512
		}
		kc.GPU = &krun.GPU{WindowMiB: window, Virgl: caps.OS == "linux"}
	}
	if cfg.GPU && caps.OS != "linux" {
		// The GPU is Vulkan alone (Venus), with no OpenGL of its own
		// (virgl): left to choose, Mesa would take virgl for it, and the
		// desktop would draw nothing. So Mesa is told what draws OpenGL,
		// for every process, the agent's sessions too (sysuser.MachineEnv):
		// zink, its OpenGL on Vulkan, where the Mac's driver lets it share
		// images with the compositor, and the CPU where not.
		if cfg.GPUZink {
			kc.Cmdline += " systemd.setenv=MESA_LOADER_DRIVER_OVERRIDE=zink"
		} else {
			kc.Cmdline += " systemd.setenv=LIBGL_ALWAYS_SOFTWARE=1"
		}
	}

	// The agent dials early in the boot, so it is listened for before the
	// monitor starts.
	if i.server, err = agent.ListenHybrid(i.vsock, guestCID); err != nil {
		return fmt.Errorf("listening for the agent: %w", err)
	}
	if cfg.SharedFiles != "" && cfg.SharedView != nil {
		kc.HostPorts[sharedFilesPort] = vsock.HybridPath(i.vsock, sharedFilesPort)
		files, err := serveSharedFiles(procCtx, i.vsock, guestCID, cfg)
		if err != nil {
			return fmt.Errorf("serving the shared files: %w", err)
		}
		i.backends = append(i.backends, files)
	}

	b, err := json.MarshalIndent(kc, "", "  ")
	if err != nil {
		return err
	}
	conf := filepath.Join(cfg.runDir(), "krun.json")
	if err := os.WriteFile(conf, b, 0o600); err != nil {
		return err
	}
	progress("booting")
	return i.launchKrun(monCtx, conf)
}

// launchKrun starts hangar-krun and watches for it to exit, which it does
// when the guest powers off or reboots.
func (i *Instance) launchKrun(ctx context.Context, conf string) error {
	bin, err := krun.Find()
	if err != nil {
		return err
	}
	logFile, err := os.OpenFile(filepath.Join(i.cfg.Dir, "monitor.log"), os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o644)
	if err != nil {
		return err
	}
	// hangar-krun stops the machine when this pipe closes: when the worker
	// exits, or dies.
	watch, held, err := os.Pipe()
	if err != nil {
		logFile.Close()
		return err
	}
	cmd := exec.CommandContext(ctx, bin, conf)
	// The GPU renders as the worker's configuration has it (ch.GPUEnv).
	cmd.Env = append(os.Environ(), i.cfg.GPUEnv...)
	cmd.SysProcAttr = sysProcAttr()
	cmd.Stdin = watch
	cmd.Stdout = logFile
	cmd.Stderr = logFile
	cmd.Cancel = func() error { return cmd.Process.Signal(syscall.SIGTERM) }
	cmd.WaitDelay = 15 * time.Second
	err = cmd.Start()
	watch.Close()
	if err != nil {
		held.Close()
		logFile.Close()
		return fmt.Errorf("starting hangar-krun: %w", err)
	}
	i.cmd = cmd
	go func() {
		err := cmd.Wait()
		held.Close()
		logFile.Close()
		if err != nil && ctx.Err() == nil {
			i.mu.Lock()
			i.err = fmt.Errorf("hangar-krun exited: %v%s", err, lastLine(filepath.Join(i.cfg.Dir, "monitor.log")))
			i.mu.Unlock()
		}
		i.cfg.dropSwap()
		close(i.exited)
	}()
	return nil
}

// DialGuest opens a connection to a port in the guest, as its monitor
// carries vsock: Cloud Hypervisor on one socket, asked for the port, and
// libkrun on a socket per port.
func (i *Instance) DialGuest(ctx context.Context, port uint32) (net.Conn, error) {
	if i.monitor == host.MonitorKrun {
		var d net.Dialer
		return d.DialContext(ctx, "unix", guestSocket(i.vsock, port))
	}
	return dialGuest(ctx, i.vsock, port)
}

// errNoKrunResize is what a resize under libkrun meets: its memory is fixed
// at boot.
var errNoKrunResize = errors.New("libkrun cannot change a running machine's memory; it takes the new size when it next starts")
