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
// out of its file, so this falls as it does.
func guestRAM(pid int) int64 {
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
