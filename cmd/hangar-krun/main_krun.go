//go:build darwin && krun

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
	"net"
	"os"
)

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: hangar-krun CONFIG.json")
		os.Exit(2)
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
	var c Config
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
	// Only returns if the machine could not start; once it has, libkrun
	// exits the process when the guest does.
	return check("starting", C.krun_start_enter(id))
}
