// Package vscode runs VS Code's server in the guest and connects the host to
// it.
//
// The server is Hangar's own build (editor/build.sh), supplied by the node
// as a read-only disk labelled DiskLabel, so every environment runs the
// editor its worker carries whatever its image is. It listens on a unix
// socket only the guest can reach; each connection the host opens on Port is
// joined to that socket as it stands, so HTTP and WebSocket alike pass
// through untouched.
//
// It runs without a connection token. Nothing outside the guest reaches the
// socket except through the host, and the control plane has already decided
// who may.
package vscode

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"os"
	"os/exec"
	"os/user"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/sysuser"
)

// Port is the vsock port the guest agent serves the editor on.
const Port uint32 = 8102

// DiskLabel is the filesystem label of the disk the editor arrives on.
const DiskLabel = "hangar-editor"

// ErrNoEditor is returned when the node supplied no editor.
var ErrNoEditor = errors.New("this environment has no editor: its worker supplies none")

const (
	mountPoint = "/run/hangar/editor"
	socketDir  = "/run/hangar/vscode"
	logFile    = "/var/log/hangar-vscode.log"
	// startTimeout covers a cold start: Node loading the server from a disk
	// not yet in the page cache.
	startTimeout = 60 * time.Second
)

// Server is the VS Code server for one user, started when first needed and
// again whenever it has exited.
type Server struct {
	user string
	log  *slog.Logger

	mu      sync.Mutex
	running *process
}

type process struct {
	socket string
	exited chan struct{}
}

func NewServer(user string, log *slog.Logger) *Server {
	return &Server{user: user, log: log}
}

// Serve joins conn to the server, starting it if need be, and returns once
// either side closes.
func (s *Server) Serve(conn io.ReadWriteCloser) {
	defer conn.Close()
	ctx, cancel := context.WithTimeout(context.Background(), startTimeout)
	socket, err := s.ensure(ctx)
	cancel()
	if err != nil {
		s.log.Warn("starting the editor", "err", err)
		fmt.Fprintf(conn, "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n%s\n", err)
		return
	}
	upstream, err := net.Dial("unix", socket)
	if err != nil {
		s.log.Warn("connecting to the editor", "err", err)
		return
	}
	defer upstream.Close()
	done := make(chan struct{}, 2)
	go func() { io.Copy(upstream, conn); done <- struct{}{} }()
	go func() { io.Copy(conn, upstream); done <- struct{}{} }()
	<-done
}

// ensure returns the socket of a running server, starting one if there is
// none.
func (s *Server) ensure(ctx context.Context) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if p := s.running; p != nil {
		select {
		case <-p.exited:
		default:
			return p.socket, nil
		}
	}
	p, err := s.start(ctx)
	if err != nil {
		return "", err
	}
	s.running = p
	return p.socket, nil
}

func (s *Server) start(ctx context.Context) (*process, error) {
	if err := mountEditor(); err != nil {
		return nil, err
	}
	u, err := user.Lookup(s.user)
	if err != nil {
		return nil, err
	}
	uid, _ := strconv.Atoi(u.Uid)
	gid, _ := strconv.Atoi(u.Gid)
	if err := os.MkdirAll(socketDir, 0o700); err != nil {
		return nil, err
	}
	if err := os.Chown(socketDir, uid, gid); err != nil {
		return nil, err
	}
	socket := filepath.Join(socketDir, "server.sock")
	_ = os.Remove(socket)

	logOut, err := os.OpenFile(logFile, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return nil, err
	}
	defer logOut.Close()

	cmd := exec.Command(filepath.Join(mountPoint, "bin", "code-server-oss"),
		"--socket-path", socket,
		"--without-connection-token",
		"--accept-server-license-terms",
		"--disable-telemetry",
	)
	cmd.Dir = u.HomeDir
	cmd.Env = []string{
		"HOME=" + u.HomeDir,
		"USER=" + u.Username,
		"LOGNAME=" + u.Username,
		"SHELL=" + sysuser.LoginShell(u.Username),
		"PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
		"LANG=C.UTF-8",
	}
	cmd.Stdout = logOut
	cmd.Stderr = logOut
	if err := sysuser.RunAs(cmd, u); err != nil {
		return nil, err
	}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("starting the editor: %w", err)
	}
	p := &process{socket: socket, exited: make(chan struct{})}
	go func() {
		err := cmd.Wait()
		s.log.Warn("the editor exited", "err", err)
		close(p.exited)
	}()
	s.log.Info("editor started", "pid", cmd.Process.Pid)

	// Ready once it answers on its socket.
	for {
		if c, err := net.Dial("unix", socket); err == nil {
			c.Close()
			return p, nil
		}
		select {
		case <-p.exited:
			return nil, fmt.Errorf("the editor exited while starting; see %s", logFile)
		case <-ctx.Done():
			cmd.Process.Kill()
			return nil, fmt.Errorf("the editor did not start in %s; see %s", startTimeout, logFile)
		case <-time.After(100 * time.Millisecond):
		}
	}
}

// mountEditor mounts the node's editor disk, read-only, if it is not
// mounted already.
func mountEditor() error {
	if mounts, err := os.ReadFile("/proc/mounts"); err == nil {
		for _, line := range strings.Split(string(mounts), "\n") {
			if f := strings.Fields(line); len(f) > 1 && f[1] == mountPoint {
				return nil
			}
		}
	}
	dev, err := filepath.EvalSymlinks(filepath.Join("/dev/disk/by-label", DiskLabel))
	if err != nil {
		return ErrNoEditor
	}
	if err := os.MkdirAll(mountPoint, 0o755); err != nil {
		return err
	}
	if out, err := exec.Command("mount", "-t", "ext4", "-o", "ro", dev, mountPoint).CombinedOutput(); err != nil {
		return fmt.Errorf("mounting the editor: %v: %s", err, strings.TrimSpace(string(out)))
	}
	return nil
}
