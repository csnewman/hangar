package worker

import (
	"encoding/binary"
	"os"
	"path/filepath"

	"golang.org/x/sys/unix"

	"github.com/csnewman/hangar/internal/api"
)

// readSystem reads the machine's memory, load, disk and cumulative CPU time
// from sysctl, Mach and statfs.
func readSystem(path string) (*api.WorkerStats, cpuSample, bool) {
	st := &api.WorkerStats{}
	cpu := cpuTimes()

	// struct loadavg: three fixed-point averages, then their scale.
	if b, err := unix.SysctlRaw("vm.loadavg"); err == nil && len(b) >= 24 {
		if scale := binary.LittleEndian.Uint64(b[16:]); scale > 0 {
			st.Load1 = float64(binary.LittleEndian.Uint32(b[0:])) / float64(scale)
		}
	}

	if total, err := unix.SysctlUint64("hw.memsize"); err == nil {
		st.MemoryTotalMiB = int(total >> 20)
		// The share of memory the kernel counts as available, as its
		// memory pressure gauge reports it.
		if level, err := unix.SysctlUint32("kern.memorystatus_level"); err == nil && level <= 100 {
			st.MemoryUsedMiB = int(total>>20) * int(100-level) / 100
		}
	}

	for path != "/" && path != "." && path != "" {
		if _, err := os.Stat(path); err == nil {
			break
		}
		path = filepath.Dir(path)
	}
	if path == "" || path == "." {
		path = "/"
	}
	var fs unix.Statfs_t
	if err := unix.Statfs(path, &fs); err == nil {
		st.DiskTotalBytes = int64(fs.Blocks) * int64(fs.Bsize)
		st.DiskUsedBytes = int64(fs.Blocks-fs.Bavail) * int64(fs.Bsize)
	}
	return st, cpu, true
}
