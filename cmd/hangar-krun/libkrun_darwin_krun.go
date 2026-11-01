//go:build krun && darwin

package main

/*
#include <libkrun.h>
*/
import "C"

import (
	"errors"
	"os"
	"unsafe"

	"golang.org/x/sys/unix"

	"github.com/csnewman/hangar/internal/krun"
)

const (
	getTermios = unix.TIOCGETA
	setTermios = unix.TIOCSETA
)

// deviceManager puts every device on virtio-mmio: libkrun has no virtio-pci
// on macOS.
func deviceManager() (C.KrunDeviceManager, func(unsafe.Pointer)) {
	devices := C.krun_mmio_device_manager_new()
	return C.KrunDeviceManager(devices), func(d unsafe.Pointer) {
		C.krun_mmio_device_manager_add(devices, C.KrunAttachDevice(d))
	}
}

// addMemory refuses a virtio-mem region: libkrun has none on macOS.
func addMemory(c krun.Config, _ func(unsafe.Pointer)) error {
	if c.HotplugMiB > 0 {
		return errors.New("libkrun has no hotpluggable memory on macOS")
	}
	return nil
}

// renderServerEnv lets the render server find the Vulkan loader bundled in
// dir, which it opens by name.
func renderServerEnv(dir string) {
	os.Setenv("DYLD_FALLBACK_LIBRARY_PATH", dir+":"+os.Getenv("DYLD_FALLBACK_LIBRARY_PATH"))
}
