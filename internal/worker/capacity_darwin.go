package worker

import (
	"runtime"

	"golang.org/x/sys/unix"

	"github.com/csnewman/hangar/internal/api"
)

func machineCapacity() (api.Resources, error) {
	b, err := unix.SysctlUint64("hw.memsize")
	if err != nil {
		return api.Resources{}, err
	}
	return api.Resources{CPUs: runtime.NumCPU(), MemoryMiB: int(b >> 20)}, nil
}

// swapMiB is 0: macOS grows its swap as it likes, so none is counted on to
// overcommit memory.
func swapMiB() int { return 0 }
