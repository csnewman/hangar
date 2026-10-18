//go:build darwin && krun

// Command hangar-krun runs one machine under libkrun, from the JSON
// description the worker writes (krun.Config). libkrun takes the process
// over, and it exits when the guest does.
package main

/*
#cgo LDFLAGS: -lkrun
#include <stdlib.h>
#include <libkrun.h>
*/
import "C"

import (
	"encoding/json"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"golang.org/x/sys/unix"

	"github.com/csnewman/hangar/internal/krun"
)

func main() {
	gpu := C.krun_has_feature(C.KRUN_FEATURE_GPU) == 1
	var vulkan string
	if gpu {
		vulkan = chooseVulkan()
	}
	if len(os.Args) == 2 && os.Args[1] == "-features" {
		json.NewEncoder(os.Stdout).Encode(krun.Features{GPU: gpu, Vulkan: vulkan})
		return
	}
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: hangar-krun CONFIG.json | -features")
		os.Exit(2)
	}
	// The worker holds the other end of standard input: when the worker
	// goes, however it goes, so does the machine, rather than running on
	// with nothing to supervise it. macOS has no parent-death signal.
	if st, err := os.Stdin.Stat(); err == nil && st.Mode()&os.ModeNamedPipe != 0 {
		go func() {
			io.Copy(io.Discard, os.Stdin)
			fmt.Fprintln(os.Stderr, "hangar-krun: the worker has gone; stopping the machine")
			os.Exit(1)
		}()
	}
	if err := run(os.Args[1]); err != nil {
		fmt.Fprintf(os.Stderr, "hangar-krun: %v\n", err)
		os.Exit(1)
	}
}

func cstr(s string) *C.char {
	// The strings are libkrun's for as long as the process lives.
	return C.CString(s)
}

func check(what string, rc C.int32_t) error {
	if rc < 0 {
		return fmt.Errorf("%s: %w", what, syscallErr(-int(rc)))
	}
	return nil
}

func run(path string) error {
	b, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	var c krun.Config
	if err := json.Unmarshal(b, &c); err != nil {
		return fmt.Errorf("%s: %w", path, err)
	}
	if level := os.Getenv("HANGAR_KRUN_LOG"); level != "" {
		var l uint32
		fmt.Sscan(level, &l)
		C.krun_set_log_level(C.uint32_t(l))
	}
	ctx := C.krun_create_ctx()
	if ctx < 0 {
		return check("creating a context", ctx)
	}
	id := C.uint32_t(ctx)
	if err := check("sizing the machine", C.krun_set_vm_config(id, C.uint8_t(c.CPUs), C.uint32_t(c.MemoryMiB))); err != nil {
		return err
	}
	if err := check("loading the kernel", C.krun_set_kernel(id, cstr(c.Kernel), C.KRUN_KERNEL_FORMAT_RAW,
		cstr(c.Initrd), cstr(c.Cmdline))); err != nil {
		return err
	}
	if c.Console != "" {
		if err := check("the console", C.krun_set_console_output(id, cstr(c.Console))); err != nil {
			return err
		}
	}
	for _, d := range c.Disks {
		if err := check("disk "+d.ID, C.krun_add_disk2(id, cstr(d.ID), cstr(d.Path),
			C.KRUN_DISK_FORMAT_RAW, C.bool(d.ReadOnly))); err != nil {
			return err
		}
	}
	// vsock without TSI, libkrun's socket redirection, which needs its own
	// patched guest kernel: the guest's sockets are its own.
	if err := check("vsock", C.krun_disable_implicit_vsock(id)); err != nil {
		return err
	}
	if err := check("vsock", C.krun_add_vsock(id, 0)); err != nil {
		return err
	}
	for port, sock := range c.HostPorts {
		if err := check(fmt.Sprintf("vsock port %d", port), C.krun_add_vsock_port2(id, C.uint32_t(port), cstr(sock), false)); err != nil {
			return err
		}
	}
	for port, sock := range c.GuestPorts {
		os.Remove(sock)
		if err := check(fmt.Sprintf("vsock port %d", port), C.krun_add_vsock_port2(id, C.uint32_t(port), cstr(sock), true)); err != nil {
			return err
		}
	}
	if c.Net != nil {
		mac, err := net.ParseMAC(c.Net.MAC)
		if err != nil || len(mac) != 6 {
			return fmt.Errorf("the MAC address %q", c.Net.MAC)
		}
		cmac := (*C.uint8_t)(C.CBytes(mac))
		if err := check("the network", C.krun_add_net_unixgram(id, cstr(c.Net.Socket), -1, cmac,
			0, C.NET_FLAG_VFKIT)); err != nil {
			return err
		}
	}
	if c.GPU != nil {
		flags := C.uint32_t(C.VIRGLRENDERER_VENUS | C.VIRGLRENDERER_NO_VIRGL)
		if err := check("the GPU", C.krun_set_gpu_options2(id, flags, C.uint64_t(c.GPU.WindowMiB)<<20)); err != nil {
			return err
		}
	}
	// Only returns if the machine could not start; once it has, libkrun
	// exits the process when the guest does.
	return check("starting", C.krun_start_enter(id))
}

// chooseVulkan picks the Vulkan driver the guest's GPU renders with from
// those bundled beside hangar-krun (bin/vulkan): KosmicKrisp on macOS 26
// and later, which it needs, and MoltenVK otherwise. VK_DRIVER_FILES, set
// already, names another; the loader reads it when virglrenderer starts.
func chooseVulkan() string {
	if f := os.Getenv("VK_DRIVER_FILES"); f != "" {
		if strings.Contains(strings.ToLower(f), "kosmickrisp") {
			return krun.VulkanKosmicKrisp
		}
		return krun.VulkanMoltenVK
	}
	exe, err := os.Executable()
	if err != nil {
		return ""
	}
	dir := filepath.Join(filepath.Dir(exe), "vulkan")
	choice := krun.VulkanMoltenVK
	if v, err := unix.Sysctl("kern.osproductversion"); err == nil {
		if major, _ := strconv.Atoi(strings.SplitN(v, ".", 2)[0]); major >= 26 {
			if _, err := os.Stat(filepath.Join(dir, "kosmickrisp.json")); err == nil {
				choice = krun.VulkanKosmicKrisp
			}
		}
	}
	os.Setenv("VK_DRIVER_FILES", filepath.Join(dir, choice+".json"))
	return choice
}
