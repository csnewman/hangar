package vsock

import (
	"errors"
	"fmt"
	"os"
	"syscall"
	"time"
	"unsafe"
)

// acceptPoll is how often Accept re-checks for a connection on a socket the
// runtime's poller cannot wait on. Every listener here can be polled, so it
// is a fallback only: in a guest, waking a few times a second for each of
// the agent's listeners is a measurable share of an idle CPU.
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

// afVSOCK is AF_VSOCK. It is defined here rather than taken from the syscall
// package because Go generates that package's constants per architecture and
// only some of them carry it: syscall.AF_VSOCK exists on linux/arm64 but not
// on linux/amd64. The value is 40 on every Linux architecture, since the
// address families are generic rather than arch-specific.
const afVSOCK = 40

// sockaddrVM mirrors struct sockaddr_vm from linux/vm_sockets.h. The layout is
// asserted by a test rather than assumed: it is 16 bytes, with the port at
// offset 4 and the CID at offset 8.
type sockaddrVM struct {
	family    uint16
	reserved1 uint16
	port      uint32
	cid       uint32
	flags     uint8
	zero      [3]uint8
}

func (sa *sockaddrVM) raw() (unsafe.Pointer, uintptr) {
	return unsafe.Pointer(sa), unsafe.Sizeof(*sa)
}

// socket opens an AF_VSOCK stream socket with close-on-exec set, so a socket
// never leaks into a command the agent runs.
func socket() (int, error) {
	fd, err := syscall.Socket(afVSOCK, syscall.SOCK_STREAM|syscall.SOCK_CLOEXEC, 0)
	if err != nil {
		return -1, fmt.Errorf("vsock socket: %w", err)
	}
	return fd, nil
}

// Dial connects to cid:port. A guest reaches its host with CIDHost.
//
// The returned *os.File is the connection: AF_VSOCK has no net.Conn in the
// standard library, and an *os.File already carries Read, Write, Close and
// deadlines, which is everything the agent protocol needs.
func Dial(cid, port uint32) (*os.File, error) {
	fd, err := socket()
	if err != nil {
		return nil, err
	}
	sa := sockaddrVM{family: afVSOCK, cid: cid, port: port}
	ptr, size := sa.raw()
	if _, _, errno := syscall.Syscall(syscall.SYS_CONNECT, uintptr(fd), uintptr(ptr), size); errno != 0 {
		syscall.Close(fd)
		return nil, fmt.Errorf("vsock connect to cid %d port %d: %w", cid, port, errno)
	}
	// Hand the runtime a non-blocking socket so the returned *os.File is
	// registered with the poller and SetDeadline works on it. Connecting
	// first and switching afterwards avoids dealing with EINPROGRESS.
	if err := syscall.SetNonblock(fd, true); err != nil {
		syscall.Close(fd)
		return nil, fmt.Errorf("vsock set nonblocking: %w", err)
	}
	return os.NewFile(uintptr(fd), fmt.Sprintf("vsock:%d:%d", cid, port)), nil
}

// Listener accepts vsock connections.
type Listener struct {
	f    *os.File
	port uint32
}

// Listen binds to cid:port and accepts connections. A host passes CIDAny so
// that one listener serves every guest.
func Listen(cid, port uint32) (*Listener, error) {
	fd, err := socket()
	if err != nil {
		return nil, err
	}
	sa := sockaddrVM{family: afVSOCK, cid: cid, port: port}
	ptr, size := sa.raw()
	if _, _, errno := syscall.Syscall(syscall.SYS_BIND, uintptr(fd), uintptr(ptr), size); errno != 0 {
		syscall.Close(fd)
		return nil, fmt.Errorf("vsock bind cid %d port %d: %w", cid, port, errno)
	}
	if err := syscall.Listen(fd, 16); err != nil {
		syscall.Close(fd)
		return nil, fmt.Errorf("vsock listen: %w", err)
	}
	// Non-blocking, so Accept waits in the runtime's poller and can give
	// up. A blocking accept(2) cannot be bounded, which would leave a host
	// waiting forever on a guest that never comes up -- and holding the port
	// against the next attempt.
	if err := syscall.SetNonblock(fd, true); err != nil {
		syscall.Close(fd)
		return nil, fmt.Errorf("vsock set nonblocking: %w", err)
	}
	return &Listener{f: os.NewFile(uintptr(fd), fmt.Sprintf("vsock-listen:%d", port)), port: port}, nil
}

// Accept waits up to timeout for the next connection and returns it with the
// CID that opened it. The CID comes from the kernel, so it identifies which
// environment is calling without trusting anything the peer says about itself.
//
// A zero or negative timeout waits indefinitely.
func (l *Listener) Accept(timeout time.Duration) (*os.File, uint32, error) {
	var sa sockaddrVM
	ptr, size := sa.raw()
	fd, err := accept(l.f, timeout, ptr, size)
	if errors.Is(err, ErrAcceptTimeout) {
		return nil, 0, err
	}
	if err != nil {
		return nil, 0, fmt.Errorf("vsock accept: %w", err)
	}
	return os.NewFile(fd, fmt.Sprintf("vsock:%d:%d", sa.cid, sa.port)), sa.cid, nil
}

// Close stops the listener. A blocked Accept fails once the socket is closed.
func (l *Listener) Close() error {
	return l.f.Close()
}
