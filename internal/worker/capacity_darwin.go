package worker

import (
	"runtime"

	"golang.org/x/sys/unix"

	"github.com/csnewman/hangar/internal/api"
)

func machineCapacity() (api.Resources, error) {
	mem, err := unix.SysctlUint64("hw.memsize")
	if err != nil {
		return api.Resources{}, err
	}
	return api.Resources{CPUs: runtime.NumCPU(), MemoryMiB: int(mem >> 20)}, nil
}
