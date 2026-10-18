package vsock

import (
	"errors"
	"fmt"
	"net"
	"os"
	"time"
)

// HybridListener accepts connections a guest makes to a port, from a
// monitor that carries vsock on unix sockets: libkrun dials "<base>_<port>"
// when the guest connects to port. As on Linux, the socket belongs to one
// guest, so a connection on it came from that environment, and the CID
// Accept reports is the host's own allocation.
type HybridListener struct {
	l    *net.UnixListener
	path string
	cid  uint32
}

// ListenHybrid binds the unix socket the monitor dials when the guest
// connects to port. base is the monitor's vsock socket path.
func ListenHybrid(base string, port, cid uint32) (*HybridListener, error) {
	path := HybridPath(base, port)
	// sockaddr_un's path is 104 bytes on macOS, terminator included.
	if len(path) >= 104 {
		return nil, fmt.Errorf("agent socket path is too long (%d bytes, limit 103): %s", len(path), path)
	}
	if fi, err := os.Lstat(path); err == nil {
		if fi.Mode()&os.ModeSocket == 0 {
			return nil, fmt.Errorf("%s exists and is not a socket", path)
		}
		if err := os.Remove(path); err != nil {
			return nil, fmt.Errorf("removing a stale agent socket: %w", err)
		}
	}
	l, err := net.ListenUnix("unix", &net.UnixAddr{Name: path, Net: "unix"})
	if err != nil {
		return nil, fmt.Errorf("binding the agent socket at %s: %w", path, err)
	}
	l.SetUnlinkOnClose(true)
	return &HybridListener{l: l, path: path, cid: cid}, nil
}

// HybridPath is where the monitor expects the host to be listening for
// connections the guest makes to port.
func HybridPath(base string, port uint32) string {
	return fmt.Sprintf("%s_%d", base, port)
}

// Accept waits up to timeout (indefinitely if zero or negative) for the
// guest to connect, and returns the connection with the environment's CID.
func (l *HybridListener) Accept(timeout time.Duration) (*os.File, uint32, error) {
	var deadline time.Time
	if timeout > 0 {
		deadline = time.Now().Add(timeout)
	}
	l.l.SetDeadline(deadline)
	c, err := l.l.AcceptUnix()
	if err != nil {
		if errors.Is(err, os.ErrDeadlineExceeded) {
			return nil, 0, ErrAcceptTimeout
		}
		return nil, 0, fmt.Errorf("accepting the agent connection: %w", err)
	}
	f, err := c.File()
	c.Close()
	if err != nil {
		return nil, 0, err
	}
	return f, l.cid, nil
}

// Close stops the listener and removes the socket.
func (l *HybridListener) Close() error { return l.l.Close() }
