//go:build krun && linux

package main

/*
#include <libkrun.h>
*/
import "C"

import (
	"unsafe"

	"golang.org/x/sys/unix"

	"github.com/csnewman/hangar/internal/krun"
)

const (
	getTermios = unix.TCGETS
	setTermios = unix.TCSETS
)

// deviceManager puts every device on virtio-pci, with an MSI-X vector for
// each queue: virtio-mmio's interrupt costs two exits to this process each
// time it fires, which nested virtualization makes dear. libkrun puts a
// device with shared memory, the GPU, on virtio-mmio itself. Devices take
// PCI slots in the order they are added, so the disks keep theirs.
func deviceManager() (C.KrunDeviceManager, func(unsafe.Pointer)) {
	devices := C.krun_pci_device_manager_new()
	return C.KrunDeviceManager(devices), func(d unsafe.Pointer) {
		C.krun_pci_device_manager_add(devices, C.KrunAttachDevice(d))
	}
}

// addMemory gives the guest a virtio-mem region, if c has one.
func addMemory(c krun.Config, add func(unsafe.Pointer)) error {
	var e C.KrunError
	if c.HotplugMiB > 0 {
		m := C.krun_mem_device_new(C.uint32_t(c.HotplugMiB), &e)
		if e != nil {
			return failed("the hotpluggable memory", e)
		}
		mem := C.krun_mem_device_handle(m)
		// A restored machine's device has what it had when it was suspended.
		if c.PluggedMiB > 0 && c.Restore == "" {
			if C.krun_mem_device_handle_set_requested_mib(mem, C.uint32_t(c.PluggedMiB), &e); e != nil {
				return failed("plugging memory in", e)
			}
		}
		plug = func(mib int) error {
			var e C.KrunError
			if C.krun_mem_device_handle_set_requested_mib(mem, C.uint32_t(mib), &e); e != nil {
				return failed("plugging memory in", e)
			}
			return nil
		}
		plugged = func() (int, int, error) {
			return int(C.krun_mem_device_handle_plugged_mib(mem)), int(C.krun_mem_device_handle_requested_mib(mem)), nil
		}
		add(unsafe.Pointer(m))
	}
	return nil
}

// renderServerEnv does nothing: the render server finds the system's
// Vulkan loader.
func renderServerEnv(string) {}
