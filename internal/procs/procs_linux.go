package procs

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"os/user"
	"runtime"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// clockTicks is USER_HZ, the unit /proc counts CPU time in: 100 on every
// architecture Linux runs guests on.
const clockTicks = 100

func (s *Server) list() ([]Process, int, int64, error) {
	entries, err := os.ReadDir("/proc")
	if err != nil {
		return nil, 0, 0, err
	}
	boot := bootTime()
	users := map[string]string{}
	now := time.Now()

	s.mu.Lock()
	defer s.mu.Unlock()
	elapsed := now.Sub(s.at).Seconds()
	first := s.at.IsZero()
	next := map[int]sample{}
	var out []Process
	for _, e := range entries {
		pid, err := strconv.Atoi(e.Name())
		if err != nil {
			continue
		}
		p, smp, ok := readProcess(pid, boot, users)
		if !ok {
			continue
		}
		next[pid] = smp
		p.Protected = protected(pid)
		// The same PID must be the same process: one that exited and whose
		// number was reused starts afresh.
		if prev, ok := s.prev[pid]; ok && !first && prev.start == smp.start && elapsed > 0 {
			p.CPUPercent = float64(smp.ticks-prev.ticks) / clockTicks / elapsed * 100
		}
		out = append(out, p)
	}
	s.prev, s.at = next, now
	return out, runtime.NumCPU(), memTotal(), nil
}

// readProcess reads one process, or reports it gone.
func readProcess(pid int, boot time.Time, users map[string]string) (Process, sample, bool) {
	dir := "/proc/" + strconv.Itoa(pid)
	stat, err := os.ReadFile(dir + "/stat")
	if err != nil {
		return Process{}, sample{}, false
	}
	// The name is in parentheses and may itself contain them and spaces,
	// so the fields are counted from the last closing one.
	open, closing := bytes.IndexByte(stat, '('), bytes.LastIndexByte(stat, ')')
	if open < 0 || closing < open {
		return Process{}, sample{}, false
	}
	p := Process{PID: pid, Name: string(stat[open+1 : closing])}
	f := strings.Fields(string(stat[closing+1:]))
	if len(f) < 22 {
		return Process{}, sample{}, false
	}
	// Fields from state, which is the third of stat's, on.
	p.State = f[0]
	p.PPID, _ = strconv.Atoi(f[1])
	utime, _ := strconv.ParseUint(f[11], 10, 64)
	stime, _ := strconv.ParseUint(f[12], 10, 64)
	p.Threads, _ = strconv.Atoi(f[17])
	start, _ := strconv.ParseUint(f[19], 10, 64)
	p.Started = boot.Add(time.Duration(start) * time.Second / clockTicks)
	rssPages, _ := strconv.ParseInt(f[21], 10, 64)
	p.RSSBytes = rssPages * int64(os.Getpagesize())

	if cmd, err := os.ReadFile(dir + "/cmdline"); err == nil {
		p.Command = strings.TrimSpace(strings.ReplaceAll(string(cmd), "\x00", " "))
	}
	if p.Command == "" {
		// A kernel thread has no command line.
		p.Command = "[" + p.Name + "]"
	}
	var st syscall.Stat_t
	if syscall.Stat(dir, &st) == nil {
		uid := strconv.Itoa(int(st.Uid))
		name, ok := users[uid]
		if !ok {
			name = uid
			if u, err := user.LookupId(uid); err == nil {
				name = u.Username
			}
			users[uid] = name
		}
		p.User = name
	}
	return p, sample{ticks: utime + stime, start: start}, true
}

func bootTime() time.Time {
	b, err := os.ReadFile("/proc/stat")
	if err != nil {
		return time.Time{}
	}
	for _, line := range strings.Split(string(b), "\n") {
		if v, ok := strings.CutPrefix(line, "btime "); ok {
			secs, _ := strconv.ParseInt(strings.TrimSpace(v), 10, 64)
			return time.Unix(secs, 0)
		}
	}
	return time.Time{}
}

func memTotal() int64 {
	b, err := os.ReadFile("/proc/meminfo")
	if err != nil {
		return 0
	}
	for _, line := range strings.Split(string(b), "\n") {
		if v, ok := strings.CutPrefix(line, "MemTotal:"); ok {
			kb, _ := strconv.ParseInt(strings.TrimSpace(strings.TrimSuffix(strings.TrimSpace(v), "kB")), 10, 64)
			return kb << 10
		}
	}
	return 0
}

// kill signals a process. The guest's init and the agent itself are
// refused: without either the environment is gone.
func protected(pid int) bool { return pid <= 1 || pid == os.Getpid() }

func kill(pid int, signal string) error {
	if protected(pid) {
		return errors.New("that process keeps the environment running, and cannot be stopped from here")
	}
	sig := syscall.SIGTERM
	switch signal {
	case "", "TERM":
	case "KILL":
		sig = syscall.SIGKILL
	default:
		return fmt.Errorf("unknown signal %q", signal)
	}
	if err := syscall.Kill(pid, sig); err != nil {
		if errors.Is(err, syscall.ESRCH) {
			return errors.New("no such process: it has already exited")
		}
		return err
	}
	return nil
}
