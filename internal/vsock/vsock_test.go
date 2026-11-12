package vsock_test

import (
	"testing"

	"github.com/csnewman/hangar/internal/vsock"
)

// The reserved context IDs are linux/vm_sockets.h's: a guest dials CIDHost
// whatever host it runs on, so a wrong number reaches nothing.
func TestReservedCIDs(t *testing.T) {
	for _, c := range []struct {
		name      string
		got, want uint32
	}{
		{"VMADDR_CID_HYPERVISOR", vsock.CIDHypervisor, 0},
		{"VMADDR_CID_LOCAL", vsock.CIDLocal, 1},
		{"VMADDR_CID_HOST", vsock.CIDHost, 2},
		{"VMADDR_CID_ANY", vsock.CIDAny, 0xFFFFFFFF},
	} {
		if c.got != c.want {
			t.Errorf("%s = %d, want %d", c.name, c.got, c.want)
		}
	}
	if vsock.FirstGuestCID <= vsock.CIDHost {
		t.Errorf("FirstGuestCID = %d, must be above CIDHost (%d)", vsock.FirstGuestCID, vsock.CIDHost)
	}
}
