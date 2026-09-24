package vsock

import (
	"fmt"
	"os"
	"syscall"
	"time"
	"unsafe"
)

// acceptPoll is how often a blocked Accept re-checks for a connection. The
// sockets are non-blocking so that Accept can honour a deadline at all; the
// interval only bounds how late it notices, and an agent connecting during
// boot does not care about a few milliseconds.
const acceptPoll = 20 * time.Millisecond

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
	fd   int
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
	// Non-blocking, so Accept can give up. A blocking accept(2) cannot be
	// bounded, which would leave a host waiting forever on a guest that never
	// comes up -- and holding the port against the next attempt.
	if err := syscall.SetNonblock(fd, true); err != nil {
		syscall.Close(fd)
		return nil, fmt.Errorf("vsock set nonblocking: %w", err)
	}
	return &Listener{fd: fd, port: port}, nil
}

// Accept waits up to timeout for the next connection and returns it with the
// CID that opened it. The CID comes from the kernel, so it identifies which
// environment is calling without trusting anything the peer says about itself.
//
// A zero or negative timeout waits indefinitely.
func (l *Listener) Accept(timeout time.Duration) (*os.File, uint32, error) {
	var deadline time.Time
	if timeout > 0 {
		deadline = time.Now().Add(timeout)
	}
	for {
		var sa sockaddrVM
		ptr, size := sa.raw()
		length := size
		fd, _, errno := syscall.Syscall6(syscall.SYS_ACCEPT4,
			uintptr(l.fd), uintptr(ptr), uintptr(unsafe.Pointer(&length)),
			uintptr(syscall.SOCK_CLOEXEC|syscall.SOCK_NONBLOCK), 0, 0)
		switch errno {
		case 0:
			f := os.NewFile(fd, fmt.Sprintf("vsock:%d:%d", sa.cid, sa.port))
			return f, sa.cid, nil
		case syscall.EAGAIN, syscall.EINTR:
			// EAGAIN is the normal "nothing yet" on a non-blocking socket.
		default:
			return nil, 0, fmt.Errorf("vsock accept: %w", errno)
		}
		if !deadline.IsZero() && time.Now().After(deadline) {
			return nil, 0, ErrAcceptTimeout
		}
		time.Sleep(acceptPoll)
	}
}

// Close stops the listener. A blocked Accept fails once the socket is closed.
func (l *Listener) Close() error {
	return syscall.Close(l.fd)
}
