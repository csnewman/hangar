package vm

import (
	"bufio"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/csnewman/hangar/internal/api"
)

// sampleEvery is how often a running machine is measured.
const sampleEvery = 5 * time.Second

// counters are the cumulative figures rates are worked out from.
type counters struct {
	at                  time.Time
	cpuTicks            uint64
	gpuTicks, supTicks  uint64
	diskRead, diskWrite uint64
	netRx, netTx        uint64
}

// sample measures a running machine until it exits, keeping the latest
// figures on the machine for Observe to report.
//
// CPU time is the monitor process's on the host, which is what the guest's
// vCPUs cost. The processes that serve it beside the monitor -- its GPU
// backend with everything that starts, its file system and its network --
// are measured apart, as support: the GPU's rendering happens there, on
// the host's CPUs when it renders in software, and no vCPU limit holds it.
// Memory, disk and network come from inside the guest through its agent:
// the guest knows what it has free, and sees its own devices whichever
// backend serves them.
func (m *machine) sample(inst *Instance, cpus int) {
	var prev *counters
	t := time.NewTicker(sampleEvery)
	defer t.Stop()
	for {
		now := counters{at: time.Now()}
		st := &api.EnvironmentStats{DiskUsedBytes: allocated(filepath.Join(m.dir, "upper.ext4"))}

		if pid := inst.Pid(); pid != 0 {
			now.cpuTicks = processTicks(pid)
		}
		gpu, others := inst.supportPids()
		host := readProcs()
		if gpu != 0 {
			now.gpuTicks = host.treeTicks(gpu)
		}
		now.supTicks = now.gpuTicks
		for _, p := range others {
			now.supTicks += host.treeTicks(p)
		}
		if out, err := inst.Session().Exec(10*time.Second, "cat", "/proc/meminfo", "/proc/net/dev", "/proc/diskstats"); err == nil && out.Code == 0 {
			guest(out.Stdout, st, &now)
		}

		if prev != nil {
			secs := now.at.Sub(prev.at).Seconds()
			rate := func(a, b uint64) float64 {
				if b < a || secs <= 0 {
					return 0
				}
				return float64(b-a) / secs
			}
			if cpus > 0 {
				st.CPUPercent = 100 * rate(prev.cpuTicks, now.cpuTicks) / clockTicks / float64(cpus)
			}
			st.SupportCPUs = rate(prev.supTicks, now.supTicks) / clockTicks
			st.GPUCPUs = rate(prev.gpuTicks, now.gpuTicks) / clockTicks
			st.DiskReadBps = rate(prev.diskRead, now.diskRead)
			st.DiskWriteBps = rate(prev.diskWrite, now.diskWrite)
			st.NetRxBps = rate(prev.netRx, now.netRx)
			st.NetTxBps = rate(prev.netTx, now.netTx)
			m.mu.Lock()
			m.stats = st
			m.mu.Unlock()
		}
		prev = &now

		select {
		case <-inst.Exited():
			m.mu.Lock()
			m.stats = nil
			m.mu.Unlock()
			return
		case <-t.C:
		}
	}
}

// guest reads the guest's /proc/meminfo, /proc/net/dev and /proc/diskstats,
// concatenated.
func guest(text string, st *api.EnvironmentStats, c *counters) {
	var total, avail int
	sc := bufio.NewScanner(strings.NewReader(text))
	for sc.Scan() {
		line := sc.Text()
		if key, rest, ok := strings.Cut(line, ":"); ok {
			f := strings.Fields(rest)
			if len(f) >= 16 {
				// net/dev: "  eth0: rxbytes rxpackets ... (8 fields) txbytes ..."
				if dev := strings.TrimSpace(key); dev != "lo" {
					rx, _ := strconv.ParseUint(f[0], 10, 64)
					tx, _ := strconv.ParseUint(f[8], 10, 64)
					c.netRx += rx
					c.netTx += tx
				}
				continue
			}
			// meminfo: "MemTotal:   4028512 kB"
			if len(f) > 0 {
				kib, _ := strconv.Atoi(f[0])
				switch key {
				case "MemTotal":
					total = kib
				case "MemAvailable":
					avail = kib
				}
			}
			continue
		}
		// diskstats: major minor name reads merged sectors ms writes merged sectors ...
		f := strings.Fields(line)
		if len(f) >= 10 && isWholeDisk(f[2]) {
			r, _ := strconv.ParseUint(f[5], 10, 64)
			w, _ := strconv.ParseUint(f[9], 10, 64)
			c.diskRead += r * 512
			c.diskWrite += w * 512
		}
	}
	st.MemoryTotalMiB = total / 1024
	st.MemoryUsedMiB = (total - avail) / 1024
}

// isWholeDisk is true for the guest's virtio disks themselves, and false for
// partitions, which would count the same traffic twice.
func isWholeDisk(name string) bool {
	return len(name) == 3 && strings.HasPrefix(name, "vd")
}

// hostProcs is every process on the host, read once for a sample: its parent,
// the CPU time it has used, and that of its children that have exited.
type hostProcs map[int]struct {
	ppid          int
	ticks, reaped uint64
}

func readProcs() hostProcs {
	out := hostProcs{}
	entries, err := os.ReadDir("/proc")
	if err != nil {
		return out
	}
	for _, e := range entries {
		pid, err := strconv.Atoi(e.Name())
		if err != nil {
			continue
		}
		f := statFields(pid)
		// After the name: state(0) ppid(1) ... utime(11) stime(12)
		// cutime(13) cstime(14).
		if len(f) < 15 {
			continue
		}
		ppid, _ := strconv.Atoi(f[1])
		var v [4]uint64
		for i := range v {
			v[i], _ = strconv.ParseUint(f[11+i], 10, 64)
		}
		out[pid] = struct {
			ppid          int
			ticks, reaped uint64
		}{ppid, v[0] + v[1], v[2] + v[3]}
	}
	return out
}

// treeTicks is the CPU time a process and everything it started have used:
// those still running, and those it has waited for. A child that has not
// been waited for when it exits is lost from the count.
func (ps hostProcs) treeTicks(root int) uint64 {
	r, ok := ps[root]
	if !ok {
		return 0
	}
	total := r.ticks + r.reaped
	children := map[int][]int{}
	for pid, p := range ps {
		children[p.ppid] = append(children[p.ppid], pid)
	}
	stack := append([]int(nil), children[root]...)
	for len(stack) > 0 {
		pid := stack[len(stack)-1]
		stack = stack[:len(stack)-1]
		total += ps[pid].ticks
		stack = append(stack, children[pid]...)
	}
	return total
}

// statFields are a process's /proc/<pid>/stat fields after its name, which
// is in parentheses and may contain spaces.
func statFields(pid int) []string {
	b, err := os.ReadFile("/proc/" + strconv.Itoa(pid) + "/stat")
	if err != nil {
		return nil
	}
	s := string(b)
	i := strings.LastIndexByte(s, ')')
	if i < 0 {
		return nil
	}
	return strings.Fields(s[i+1:])
}

// processTicks is a process's user and system CPU time, in clock ticks.
func processTicks(pid int) uint64 {
	b, err := os.ReadFile("/proc/" + strconv.Itoa(pid) + "/stat")
	if err != nil {
		return 0
	}
	// The command name is in parentheses and may contain spaces, so fields
	// are counted from after its closing one.
	s := string(b)
	i := strings.LastIndexByte(s, ')')
	if i < 0 {
		return 0
	}
	f := strings.Fields(s[i+1:])
	// After the name: state(0) ... utime(11) stime(12).
	if len(f) < 13 {
		return 0
	}
	u, _ := strconv.ParseUint(f[11], 10, 64)
	sys, _ := strconv.ParseUint(f[12], 10, 64)
	return u + sys
}

// clockTicks is the kernel's USER_HZ, the unit of /proc's CPU times. It is
// 100 on every architecture Linux runs Hangar on.
const clockTicks = 100
