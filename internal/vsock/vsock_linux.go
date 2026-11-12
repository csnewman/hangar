package vsock

import (
	"errors"
	"os"
	"syscall"
	"time"
	"unsafe"
)

// acceptPoll is how often Accept re-checks for a connection on a socket the
// runtime's poller cannot wait on. Every listener here can be polled, so it
// is a fallback only.
const acceptPoll = 20 * time.Millisecond

// accept waits up to timeout (none if zero or negative) for a connection on
// f, a non-blocking listening socket, and returns the new descriptor, with
// the peer's address in sa. It waits in the runtime's poller, as a
// net.Listener does, so an idle listener costs nothing.
func accept(f *os.File, timeout time.Duration, sa unsafe.Pointer, size uintptr) (uintptr, error) {
	var deadline time.Time
	if timeout > 0 {
		deadline = time.Now().Add(timeout)
	}
	rc, err := f.SyscallConn()
	if err != nil {
		return 0, err
	}
	try := func(fd uintptr) (uintptr, syscall.Errno) {
		for {
			length := size
			nfd, _, errno := syscall.Syscall6(syscall.SYS_ACCEPT4,
				fd, uintptr(sa), uintptr(unsafe.Pointer(&length)),
				uintptr(syscall.SOCK_CLOEXEC|syscall.SOCK_NONBLOCK), 0, 0)
			if errno != syscall.EINTR {
				return nfd, errno
			}
		}
	}
	if err := f.SetReadDeadline(deadline); errors.Is(err, os.ErrNoDeadline) {
		return acceptPolling(rc, deadline, try)
	} else if err != nil {
		return 0, err
	}
	var nfd uintptr
	var aerr error
	err = rc.Read(func(fd uintptr) bool {
		var errno syscall.Errno
		nfd, errno = try(fd)
		if errno == syscall.EAGAIN {
			return false
		}
		if errno != 0 {
			aerr = errno
		}
		return true
	})
	switch {
	case errors.Is(err, os.ErrDeadlineExceeded):
		return 0, ErrAcceptTimeout
	case err != nil:
		return 0, err
	}
	return nfd, aerr
}

// acceptPolling is accept for a socket the poller cannot wait on: it tries,
// and sleeps between tries.
func acceptPolling(rc syscall.RawConn, deadline time.Time, try func(uintptr) (uintptr, syscall.Errno)) (uintptr, error) {
	for {
		var nfd uintptr
		var errno syscall.Errno
		if err := rc.Control(func(fd uintptr) { nfd, errno = try(fd) }); err != nil {
			return 0, err
		}
		switch errno {
		case 0:
			return nfd, nil
		case syscall.EAGAIN:
		default:
			return 0, errno
		}
		if !deadline.IsZero() && time.Now().After(deadline) {
			return 0, ErrAcceptTimeout
		}
		time.Sleep(acceptPoll)
	}
}
