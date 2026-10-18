//go:build darwin && cgo

package nfs

/*
#include <errno.h>
#include <stdlib.h>
#include <fcntl.h>
#include <sys/stat.h>

static int hangar_mknodat(int dir, const char *name, mode_t mode) {
	return mknodat(dir, name, mode, 0) < 0 ? errno : 0;
}

static int hangar_punchhole(int fd, off_t off, off_t len) {
	fpunchhole_t p = {0};
	p.fp_offset = off;
	p.fp_length = len;
	return fcntl(fd, F_PUNCHHOLE, &p) < 0 ? errno : 0;
}
*/
import "C"

import (
	"syscall"
	"unsafe"
)

// mknodAt makes a FIFO or socket in dir.
func mknodAt(dir int, name string, mode uint32) error {
	cname := C.CString(name)
	defer C.free(unsafe.Pointer(cname))
	if e := C.hangar_mknodat(C.int(dir), cname, C.mode_t(mode)); e != 0 {
		return syscall.Errno(e)
	}
	return nil
}

// punchHole frees whole blocks of a file, which then read as zeros.
func punchHole(fd int, off, n int64) error {
	if e := C.hangar_punchhole(C.int(fd), C.off_t(off), C.off_t(n)); e != 0 {
		return syscall.Errno(e)
	}
	return nil
}
