package vm

import (
	"bufio"
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
// from no file: its guest memory is the anonymous mapping holding it
// (processMemory).
func guestRAM(pid int) int64 {
	if n := memfdRAM(pid); n > 0 {
		return n
	}
	guest, _ := processMemory(pid)
	return guest
}

// guestMapping is the least a mapping of guest memory spans. A guest boots
// with at least 1 GiB, in one mapping under libkrun (its RAM, the region
// virtio-mem plugs into and the GPU's window beside them), and libkrun's
// own mappings, its threads' arenas and the GPU's heaps, are smaller.
const guestMapping = 1 << 30

// processMemory is the memory a process holds, from its smaps: guest, what
// it maps of a guest's RAM (Cloud Hypervisor's memory files, libkrun's
// mapping of guest memory), and other, the rest of its anonymous and shared
// memory, which is what serving the guest costs beside its RAM. Memory it
// shares with another process (a GPU's buffers, mapped by its backend and
// the monitor both) is counted in proportion (Pss), so that summing over
// the processes counts it once. The files it maps, its program and
// libraries, are the host's page cache and not counted.
func processMemory(pid int) (guest, other int64) {
	f, err := os.Open(filepath.Join("/proc", strconv.Itoa(pid), "smaps"))
	if err != nil {
		return 0, 0
	}
	defer f.Close()
	// What the current mapping is: a guest's RAM, memory (anonymous or
	// shared), or a file.
	const (
		file = iota
		memory
		ram
	)
	kind := file
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		line := sc.Text()
		fields := strings.Fields(line)
		if len(fields) == 0 {
			continue
		}
		if span, ok := mappingSpan(fields[0]); ok {
			// "start-end perms offset dev inode [name]"
			name := ""
			if len(fields) >= 6 {
				name = strings.Join(fields[5:], " ")
			}
			switch {
			case strings.HasPrefix(name, "/memfd:ch_ram"):
				kind = ram
			case name == "" && span >= guestMapping:
				kind = ram
			case name == "" || strings.HasPrefix(name, "[") || strings.HasPrefix(name, "/memfd:") ||
				strings.HasPrefix(name, "/dev/shm/") || strings.HasPrefix(name, "/SYSV") ||
				strings.HasPrefix(name, "/dev/dri/"):
				kind = memory
			default:
				kind = file
			}
			continue
		}
		var into *int64
		switch {
		case kind == ram && fields[0] == "Rss:":
			into = &guest
		case kind == memory && fields[0] == "Pss:":
			into = &other
		default:
			continue
		}
		if len(fields) >= 2 {
			kib, _ := strconv.ParseInt(fields[1], 10, 64)
			*into += kib << 10
		}
	}
	return guest, other
}

// mappingSpan is the size of the mapping a smaps header's first field
// ("start-end", in hex) describes, and whether it is one.
func mappingSpan(field string) (int64, bool) {
	a, b, ok := strings.Cut(field, "-")
	if !ok {
		return 0, false
	}
	start, err1 := strconv.ParseUint(a, 16, 64)
	end, err2 := strconv.ParseUint(b, 16, 64)
	if err1 != nil || err2 != nil || end < start {
		return 0, false
	}
	return int64(end - start), true
}

// treeMemory is what a process and everything it started hold beside a
// guest's RAM (processMemory's other).
func (ps hostProcs) treeMemory(root int) int64 {
	var total int64
	for _, pid := range ps.tree(root) {
		_, other := processMemory(pid)
		total += other
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
