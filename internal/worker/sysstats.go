package worker

import (
	"sync"

	"github.com/csnewman/hangar/internal/api"
)

// cpuSample is the machine's cumulative CPU time, in ticks: busy and total.
type cpuSample struct{ busy, total uint64 }

// sysStats measures the machine a worker runs on. CPU use is the share of
// time busy between one measurement and the next, so the first measurement
// has none to report.
type sysStats struct {
	// path is on the filesystem whose space is reported: where
	// environments are kept.
	path string

	mu   sync.Mutex
	last *cpuSample
}

func (s *sysStats) measure() *api.WorkerStats {
	st, cpu, ok := readSystem(s.path)
	if !ok {
		return nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.last != nil && cpu.total > s.last.total {
		st.CPUPercent = 100 * float64(cpu.busy-s.last.busy) / float64(cpu.total-s.last.total)
	}
	s.last = &cpu
	return st
}
