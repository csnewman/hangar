package vm

import (
	"io/fs"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
)

// sysProcAttr ties the monitor to the worker: if the worker dies without
// shutting it down, the kernel stops the monitor too, rather than leaving a
// guest running that nothing supervises or reports.
func sysProcAttr() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{Pdeathsig: syscall.SIGKILL}
}

// diskUsage is the space a file takes on disk: its allocated blocks, which
// for a sparse image is what it holds rather than how big it claims to be.
func diskUsage(fi fs.FileInfo) int64 {
	if st, ok := fi.Sys().(*syscall.Stat_t); ok {
		return st.Blocks * 512
	}
	return fi.Size()
}

// guestRAM is the host memory a monitor's guest RAM takes up: the pages
// held by the memory files the monitor maps it from, each file counted once
// however many times it is open. A page the guest hands back is punched
// out of its file, so this falls as it does. libkrun maps the guest's RAM
// from no file: its guest memory is the anonymous and shared memory the
// process holds.
func guestRAM(pid int) int64 {
	if n := memfdRAM(pid); n > 0 {
		return n
	}
	return anonRSS(pid)
}

// anonRSS is the anonymous and shared memory a process holds, leaving out
// the files it has mapped, its program and libraries.
func anonRSS(pid int) int64 {
	b, err := os.ReadFile(filepath.Join("/proc", strconv.Itoa(pid), "status"))
	if err != nil {
		return 0
	}
	var total int64
	for _, line := range strings.Split(string(b), "\n") {
		k, v, ok := strings.Cut(line, ":")
		if !ok || (k != "RssAnon" && k != "RssShmem") {
			continue
		}
		if f := strings.Fields(v); len(f) > 0 {
			kib, _ := strconv.ParseInt(f[0], 10, 64)
			total += kib << 10
		}
	}
	return total
}

func memfdRAM(pid int) int64 {
	dir := filepath.Join("/proc", strconv.Itoa(pid), "fd")
	entries, err := os.ReadDir(dir)
	if err != nil {
		return 0
	}
	seen := map[uint64]bool{}
	var total int64
	for _, e := range entries {
		fd := filepath.Join(dir, e.Name())
		if target, err := os.Readlink(fd); err != nil || !strings.HasPrefix(target, "/memfd:ch_ram") {
			continue
		}
		var st syscall.Stat_t
		if syscall.Stat(fd, &st) != nil || seen[st.Ino] {
			continue
		}
		seen[st.Ino] = true
		total += st.Blocks * 512
	}
	return total
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
