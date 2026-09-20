//go:build linux

package vsock

import (
	"testing"
	"unsafe"
)

// TestSockaddrVMLayout pins the struct to the kernel's ABI.
//
// sockaddrVM is handed to bind(2) and connect(2) as raw memory, so a wrong
// field order or size is not a compile error: the kernel reads whatever
// happens to be at those offsets and the call fails, or worse, succeeds
// against the wrong address. The numbers are struct sockaddr_vm from
// linux/vm_sockets.h.
func TestSockaddrVMLayout(t *testing.T) {
	var sa sockaddrVM

	if got, want := unsafe.Sizeof(sa), uintptr(16); got != want {
		t.Errorf("sizeof(sockaddr_vm) = %d, want %d", got, want)
	}
	for _, c := range []struct {
		name string
		got  uintptr
		want uintptr
	}{
		{"svm_family", unsafe.Offsetof(sa.family), 0},
		{"svm_reserved1", unsafe.Offsetof(sa.reserved1), 2},
		{"svm_port", unsafe.Offsetof(sa.port), 4},
		{"svm_cid", unsafe.Offsetof(sa.cid), 8},
		{"svm_flags", unsafe.Offsetof(sa.flags), 12},
	} {
		if c.got != c.want {
			t.Errorf("offsetof(%s) = %d, want %d", c.name, c.got, c.want)
		}
	}
}

// TestAFVSOCK guards the locally defined constant. The syscall package does
// not carry AF_VSOCK on every architecture, so this file owns the value; were
// it wrong, every socket() call would open a socket of some other family.
func TestAFVSOCK(t *testing.T) {
	if afVSOCK != 40 {
		t.Errorf("afVSOCK = %d, want 40", afVSOCK)
	}
}

// TestReservedCIDs checks the well-known context IDs against vm_sockets.h.
func TestReservedCIDs(t *testing.T) {
	for _, c := range []struct {
		name string
		got  uint32
		want uint32
	}{
		{"VMADDR_CID_HYPERVISOR", CIDHypervisor, 0},
		{"VMADDR_CID_LOCAL", CIDLocal, 1},
		{"VMADDR_CID_HOST", CIDHost, 2},
		{"VMADDR_CID_ANY", CIDAny, 0xFFFFFFFF},
	} {
		if c.got != c.want {
			t.Errorf("%s = %d, want %d", c.name, c.got, c.want)
		}
	}
	if FirstGuestCID <= CIDHost {
		t.Errorf("FirstGuestCID = %d, must be above CIDHost (%d)", FirstGuestCID, CIDHost)
	}
}
