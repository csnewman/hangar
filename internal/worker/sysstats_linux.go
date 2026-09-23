package worker

import (
	"bufio"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"

	"github.com/csnewman/hangar/internal/api"
)

// readSystem reads the machine's memory, load, disk and cumulative CPU time
// from /proc and statfs.
func readSystem(path string) (*api.WorkerStats, cpuSample, bool) {
	st := &api.WorkerStats{}
	var cpu cpuSample

	if b, err := os.ReadFile("/proc/stat"); err == nil {
		line, _, _ := strings.Cut(string(b), "\n")
		f := strings.Fields(line)
		// cpu user nice system idle iowait irq softirq steal ...
		if len(f) >= 8 && f[0] == "cpu" {
			var v [8]uint64
			for i := 1; i < 8 && i < len(f); i++ {
				v[i], _ = strconv.ParseUint(f[i], 10, 64)
			}
			idle := v[4] + v[5]
			for i := 1; i < 8; i++ {
				cpu.total += v[i]
			}
			cpu.busy = cpu.total - idle
		}
	}

	if b, err := os.ReadFile("/proc/loadavg"); err == nil {
		if f := strings.Fields(string(b)); len(f) > 0 {
			st.Load1, _ = strconv.ParseFloat(f[0], 64)
		}
	}

	if f, err := os.Open("/proc/meminfo"); err == nil {
		var total, avail int
		sc := bufio.NewScanner(f)
		for sc.Scan() {
			k, v, ok := strings.Cut(sc.Text(), ":")
			if !ok {
				continue
			}
			kib, _ := strconv.Atoi(strings.TrimSpace(strings.TrimSuffix(strings.TrimSpace(v), "kB")))
			switch k {
			case "MemTotal":
				total = kib
			case "MemAvailable":
				avail = kib
			}
		}
		f.Close()
		st.MemoryTotalMiB = total / 1024
		st.MemoryUsedMiB = (total - avail) / 1024
	}

	// The directory may not exist yet, before the first environment; its
	// nearest ancestor is on the same filesystem, or the one it will be on.
	for path != "/" && path != "." && path != "" {
		if _, err := os.Stat(path); err == nil {
			break
		}
		path = filepath.Dir(path)
	}
	if path == "" || path == "." {
		path = "/"
	}
	var fs syscall.Statfs_t
	if err := syscall.Statfs(path, &fs); err == nil {
		st.DiskTotalBytes = int64(fs.Blocks) * fs.Bsize
		st.DiskUsedBytes = int64(fs.Blocks-fs.Bfree) * fs.Bsize
	}
	return st, cpu, true
}
