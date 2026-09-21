package vsock

import (
	"fmt"
	"os"
	"syscall"
	"time"
	"unsafe"
)

// HybridListener accepts agent connections from a monitor that implements
// vsock in userspace rather than through the host kernel's vhost-vsock.
//
// Cloud Hypervisor is such a monitor. The guest sees an ordinary virtio-vsock
// device, but the host end is a unix socket: when the guest connects to port
// P, the monitor dials the unix socket at "<path>_<P>". So the host listens
// there instead of binding AF_VSOCK, and the guest needs no changes.
//
// Identity is preserved, by a different mechanism. On AF_VSOCK the kernel
// stamps each connection with the guest's context ID, which the guest cannot
// choose or forge. Here the path carries it: the socket belongs to one guest,
// its name is chosen by the host, and only the monitor holding that guest is
// told where it is. So a connection arriving on this listener came from that
// environment and no other, and the CID it reports is the host's own
// allocation rather than anything the peer claimed.
//
// One listener therefore serves one environment, where the AF_VSOCK listener
// serves all of them on CIDAny.
type HybridListener struct {
	fd   int
	path string
	cid  uint32
}

// ListenHybrid binds the unix socket a userspace-vsock monitor will dial when
// the guest connects to port.
//
// base is the monitor's vsock socket path, the same value it is configured
// with; the port is appended, because that is how the monitor names it.
//
// cid is reported by Accept. It identifies the environment, and it is the
// host's own allocation rather than anything read off the wire.
func ListenHybrid(base string, port, cid uint32) (*HybridListener, error) {
	path := HybridPath(base, port)

	// sockaddr_un's path is 108 bytes including its terminator, and a longer
	// one is silently truncated into a socket nobody can find.
	if len(path) >= 108 {
		return nil, fmt.Errorf("agent socket path is too long (%d bytes, limit 107): %s", len(path), path)
	}

	// A stale socket from a monitor that was killed rather than stopped would
	// otherwise make every later boot fail to bind. Only a socket is removed:
	// anything else at that path is a mistake worth reporting.
	if fi, err := os.Lstat(path); err == nil {
		if fi.Mode()&os.ModeSocket == 0 {
			return nil, fmt.Errorf("%s exists and is not a socket", path)
		}
		if err := os.Remove(path); err != nil {
			return nil, fmt.Errorf("removing a stale agent socket: %w", err)
		}
	}

	fd, err := syscall.Socket(syscall.AF_UNIX, syscall.SOCK_STREAM|syscall.SOCK_CLOEXEC, 0)
	if err != nil {
		return nil, fmt.Errorf("agent socket: %w", err)
	}
	if err := syscall.Bind(fd, &syscall.SockaddrUnix{Name: path}); err != nil {
		syscall.Close(fd)
		return nil, fmt.Errorf("binding the agent socket at %s: %w", path, err)
	}
	if err := syscall.Listen(fd, 16); err != nil {
		syscall.Close(fd)
		os.Remove(path)
		return nil, fmt.Errorf("listening on the agent socket: %w", err)
	}
	// Non-blocking so Accept can honour a deadline, exactly as the AF_VSOCK
	// listener does.
	if err := syscall.SetNonblock(fd, true); err != nil {
		syscall.Close(fd)
		os.Remove(path)
		return nil, fmt.Errorf("agent socket set nonblocking: %w", err)
	}
	return &HybridListener{fd: fd, path: path, cid: cid}, nil
}

// HybridPath is where a userspace-vsock monitor expects the host to be
// listening for connections the guest makes to port.
func HybridPath(base string, port uint32) string {
	return fmt.Sprintf("%s_%d", base, port)
}

// Accept waits up to timeout for the agent to dial out and returns the
// connection with the environment's context ID.
//
// A zero or negative timeout waits indefinitely.
func (l *HybridListener) Accept(timeout time.Duration) (*os.File, uint32, error) {
	var deadline time.Time
	if timeout > 0 {
		deadline = time.Now().Add(timeout)
	}
	for {
		var sa syscall.RawSockaddrAny
		length := uintptr(unsafe.Sizeof(sa))
		fd, _, errno := syscall.Syscall6(syscall.SYS_ACCEPT4,
			uintptr(l.fd), uintptr(unsafe.Pointer(&sa)), uintptr(unsafe.Pointer(&length)),
			uintptr(syscall.SOCK_CLOEXEC|syscall.SOCK_NONBLOCK), 0, 0)
		switch errno {
		case 0:
			return os.NewFile(fd, fmt.Sprintf("hybrid-vsock:%d", l.cid)), l.cid, nil
		case syscall.EAGAIN, syscall.EINTR:
			// EAGAIN is the normal "nothing yet" on a non-blocking socket.
		default:
			return nil, 0, fmt.Errorf("accepting the agent connection: %w", errno)
		}
		if !deadline.IsZero() && time.Now().After(deadline) {
			return nil, 0, ErrAcceptTimeout
		}
		time.Sleep(acceptPoll)
	}
}

// Close stops the listener and removes the socket.
func (l *HybridListener) Close() error {
	err := syscall.Close(l.fd)
	os.Remove(l.path)
	return err
}
