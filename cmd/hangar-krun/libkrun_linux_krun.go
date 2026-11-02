//go:build krun && linux

package main

/*
#include <libkrun.h>
*/
import "C"

import (
	"unsafe"

	"golang.org/x/sys/unix"
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

// renderServerEnv does nothing: the render server finds the system's
// Vulkan loader.
func renderServerEnv(string) {}
