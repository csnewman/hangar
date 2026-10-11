//go:build linux

package fsuser

import (
	"os"
	"runtime"
	"unsafe"

	"golang.org/x/sys/unix"
)

// Do runs fn with the calling thread's filesystem identity set to u, and
// set back to what it was after: Do within Do, as Root, acts as the
// process again. A process not running as root cannot set it, and runs fn
// as itself.
func Do(u User, fn func() error) error {
	if os.Geteuid() != 0 {
		return fn()
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	prevUID, _ := unix.SetfsuidRetUid(-1)
	prevGID, _ := unix.SetfsgidRetGid(-1)
	prevGroups, err := getgroups()
	if err != nil {
		return err
	}
	// The thread's groups through the raw call: the C library's, and
	// Go's, change every thread's.
	if err := setgroups(u.Groups); err != nil {
		return err
	}
	unix.Setfsgid(int(u.GID))
	unix.Setfsuid(int(u.UID))
	defer func() {
		unix.Setfsuid(prevUID)
		unix.Setfsgid(prevGID)
		setgroups(prevGroups)
	}()
	return fn()
}

func getgroups() ([]uint32, error) {
	n, _, e := unix.RawSyscall(unix.SYS_GETGROUPS, 0, 0, 0)
	if e != 0 {
		return nil, e
	}
	if n == 0 {
		return nil, nil
	}
	gids := make([]uint32, n)
	if _, _, e := unix.RawSyscall(unix.SYS_GETGROUPS, n, uintptr(unsafe.Pointer(&gids[0])), 0); e != 0 {
		return nil, e
	}
	return gids, nil
}

func setgroups(gids []uint32) error {
	var p unsafe.Pointer
	if len(gids) > 0 {
		p = unsafe.Pointer(&gids[0])
	}
	if _, _, e := unix.RawSyscall(unix.SYS_SETGROUPS, uintptr(len(gids)), uintptr(p), 0); e != 0 {
		return e
	}
	return nil
}
