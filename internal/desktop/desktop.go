// Package desktop serves an environment's desktop to the host as VNC.
//
// The desktop is the image's own: a sway session, headless, that draws into
// memory whether or not anyone is looking (hangar-desktop.service). The VNC
// server is wayvnc, which attaches to that session as another Wayland client
// -- it copies the screen with the screencopy protocol and injects input
// with the virtual pointer and keyboard ones -- so it can come and go without
// disturbing anything on the desktop. It is started when the host first asks
// for the desktop, and again whenever it has exited.
//
// wayvnc listens on a unix socket only the guest can reach, and requires no
// password: the control plane has already decided who may reach it.
//
// # Protocol
//
// A connection on Port starts with one JSON line, a Request. For OpVNC the
// connection is then joined to wayvnc's socket as it stands, so the host
// carries RFB without reading it. For OpResize the desktop is resized and one
// JSON line, a Reply, answers.
//
// The size is set here rather than by a viewer through RFB: wayvnc's own
// resizing asks sway for a custom mode sway refuses, and wayvnc exits when it
// does. sway sets a headless output's resolution without complaint.
package desktop

import (
	"bufio"
	"context"
	"encoding/json"
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
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/sysuser"
)

// Port is the vsock port the guest agent serves the desktop on.
const Port uint32 = 8103

// Request operations.
const (
	OpVNC    = "vnc"
	OpResize = "resize"
)

// Request opens a connection.
type Request struct {
	Op     string `json:"op"`
	Width  int    `json:"width,omitempty"`
	Height int    `json:"height,omitempty"`
}

// Reply answers an OpResize.
type Reply struct {
	Err string `json:"err,omitempty"`
}

// Limits on a desktop's size. Both sides are rounded down to even numbers,
// which video encoders want.
const (
	MinWidth  = 640
	MinHeight = 400
	MaxWidth  = 3840
	MaxHeight = 2160
)

// ClampSize fits a requested size within the limits.
func ClampSize(w, h int) (int, int) {
	w = min(max(w, MinWidth), MaxWidth) &^ 1
	h = min(max(h, MinHeight), MaxHeight) &^ 1
	return w, h
}

// ErrNoDesktop is returned when the environment has no desktop running to
// serve: it is headless, or its image has no desktop.
var ErrNoDesktop = errors.New("this environment has no desktop running")

const (
	socketDir = "/run/hangar/desktop"
	logFile   = "/var/log/hangar-desktop-vnc.log"
	// waylandDisplay is the socket sway's session listens on, in the user's
	// runtime directory; see images/*/tree/etc/hangar/sway.conf.
	waylandDisplay = "wayland-1"
	// startTimeout covers the compositor still coming up just after boot.
	startTimeout = 30 * time.Second
)

// Server is the VNC server for one user's desktop.
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

// Serve answers one connection from the host.
func (s *Server) Serve(conn io.ReadWriteCloser) {
	defer conn.Close()
	br := bufio.NewReader(conn)
	line, err := br.ReadBytes('\n')
	if err != nil {
		return
	}
	var req Request
	if err := json.Unmarshal(line, &req); err != nil {
		return
	}
	switch req.Op {
	case OpVNC:
		s.vnc(struct {
			io.Reader
			io.Writer
		}{br, conn})
	case OpResize:
		var reply Reply
		if err := s.resize(req.Width, req.Height); err != nil {
			reply.Err = err.Error()
		}
		b, _ := json.Marshal(reply)
		conn.Write(append(b, '\n'))
	}
}

// vnc joins conn to the VNC server, starting it if need be, and returns once
// either side closes. When there is no desktop, it returns without a byte
// written: the RFB handshake starts with the server's version, so a viewer
// sees a connection that ended before it began.
func (s *Server) vnc(conn io.ReadWriter) {
	ctx, cancel := context.WithTimeout(context.Background(), startTimeout)
	socket, err := s.ensure(ctx)
	cancel()
	if err != nil {
		s.log.Warn("starting the desktop's VNC server", "err", err)
		return
	}
	upstream, err := net.Dial("unix", socket)
	if err != nil {
		s.log.Warn("connecting to the desktop's VNC server", "err", err)
		return
	}
	defer upstream.Close()
	done := make(chan struct{}, 2)
	go func() { io.Copy(upstream, conn); done <- struct{}{} }()
	go func() { io.Copy(conn, upstream); done <- struct{}{} }()
	<-done
}

// resize sets the size of the desktop's output. Everyone watching sees the
// desktop change size, as with a physical monitor's resolution.
func (s *Server) resize(width, height int) error {
	width, height = ClampSize(width, height)
	u, err := user.Lookup(s.user)
	if err != nil {
		return err
	}
	runtimeDir := "/run/user/" + u.Uid
	socks, _ := filepath.Glob(filepath.Join(runtimeDir, "sway-ipc.*.sock"))
	if len(socks) == 0 {
		return ErrNoDesktop
	}
	cmd := exec.Command("swaymsg", "output", "HEADLESS-1", "resolution", fmt.Sprintf("%dx%d", width, height))
	cmd.Env = []string{
		"HOME=" + u.HomeDir,
		"XDG_RUNTIME_DIR=" + runtimeDir,
		"SWAYSOCK=" + socks[len(socks)-1],
		"PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
	}
	if err := sysuser.RunAs(cmd, u); err != nil {
		return err
	}
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("resizing the desktop: %v: %s", err, out)
	}
	return nil
}

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
	wayvnc, err := exec.LookPath("wayvnc")
	if err != nil {
		return nil, fmt.Errorf("%w: its image has no wayvnc", ErrNoDesktop)
	}
	u, err := user.Lookup(s.user)
	if err != nil {
		return nil, err
	}
	runtimeDir := "/run/user/" + u.Uid
	wayland := filepath.Join(runtimeDir, waylandDisplay)
	// The compositor may still be starting just after boot; a headless
	// environment never starts it at all.
	for {
		if _, err := os.Stat(wayland); err == nil {
			break
		}
		select {
		case <-ctx.Done():
			return nil, ErrNoDesktop
		case <-time.After(250 * time.Millisecond):
		}
	}

	uid, _ := strconv.Atoi(u.Uid)
	gid, _ := strconv.Atoi(u.Gid)
	if err := os.MkdirAll(socketDir, 0o700); err != nil {
		return nil, err
	}
	if err := os.Chown(socketDir, uid, gid); err != nil {
		return nil, err
	}
	socket := filepath.Join(socketDir, "vnc.sock")
	_ = os.Remove(socket)
	// No configuration file: wayvnc's defaults are no authentication, which
	// is what a socket only the agent reaches wants. An empty file stops it
	// reading the user's own.
	config := filepath.Join(socketDir, "config")
	if err := os.WriteFile(config, nil, 0o644); err != nil {
		return nil, err
	}

	logOut, err := os.OpenFile(logFile, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return nil, err
	}
	defer logOut.Close()

	cmd := exec.Command(wayvnc,
		"--config", config,
		"--unix-socket",
		// Its own control socket, not the default in the user's runtime
		// directory, which a wayvnc the user starts would want.
		"--socket", filepath.Join(socketDir, "ctl.sock"),
		// The desktop's size is Hangar's to set (OpResize), not a viewer's.
		"--disable-resizing",
		// The desktop has no hardware cursor for a viewer to be sent, so
		// the pointer is drawn into the picture.
		"--render-cursor",
		socket,
	)
	cmd.Dir = u.HomeDir
	cmd.Env = []string{
		"HOME=" + u.HomeDir,
		"USER=" + u.Username,
		"XDG_RUNTIME_DIR=" + runtimeDir,
		"WAYLAND_DISPLAY=" + waylandDisplay,
		"PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
	}
	cmd.Stdout = logOut
	cmd.Stderr = logOut
	if err := sysuser.RunAs(cmd, u); err != nil {
		return nil, err
	}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("starting wayvnc: %w", err)
	}
	p := &process{socket: socket, exited: make(chan struct{})}
	go func() {
		err := cmd.Wait()
		s.log.Warn("the desktop's VNC server exited", "err", err)
		close(p.exited)
	}()
	s.log.Info("desktop VNC server started", "pid", cmd.Process.Pid)

	for {
		if c, err := net.Dial("unix", socket); err == nil {
			c.Close()
			return p, nil
		}
		select {
		case <-p.exited:
			return nil, fmt.Errorf("wayvnc exited while starting; see %s", logFile)
		case <-ctx.Done():
			cmd.Process.Kill()
			return nil, fmt.Errorf("wayvnc did not start in %s; see %s", startTimeout, logFile)
		case <-time.After(100 * time.Millisecond):
		}
	}
}
