// Package code runs the Code tab's service in the guest and connects the
// host to it.
//
// The service is hangar-code (editor/code), a Node program shipped on the
// node's editor disk beside VS Code's server and run with the Node that
// ships there: it lists and watches files, answers git, and holds the files
// open for editing as shared documents. One runs per root folder, as the
// environment's user, on a unix socket.
//
// # Protocol
//
// A connection on Port starts with one JSON line, a Request, naming the root.
// After it the connection is joined to that root's service as it stands:
// the service speaks length-prefixed frames, which the host carries without
// reading.
package code

import (
	"bufio"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net"
	"os"
	"os/exec"
	"os/user"
	"path"
	"path/filepath"
	"strconv"
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/sysuser"
	"github.com/csnewman/hangar/internal/vscode"
)

// Port is the vsock port the guest agent serves the Code tab on.
const Port uint32 = 8104

// Request opens a connection.
type Request struct {
	// Root is the folder the Code tab shows. Empty is the user's home.
	Root string `json:"root,omitempty"`
}

const (
	socketDir = "/run/hangar/code"
	logFile   = "/var/log/hangar-code.log"
	// script is the service, relative to the editor disk.
	script       = "hangar/code.mjs"
	startTimeout = 30 * time.Second
)

// Server runs the service for one user, one process per root.
type Server struct {
	user string
	log  *slog.Logger

	mu      sync.Mutex
	running map[string]*process
}

type process struct {
	socket string
	exited chan struct{}
}

func NewServer(user string, log *slog.Logger) *Server {
	return &Server{user: user, log: log, running: map[string]*process{}}
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
	ctx, cancel := context.WithTimeout(context.Background(), startTimeout)
	socket, err := s.ensure(ctx, req.Root)
	cancel()
	if err != nil {
		s.log.Warn("starting the code service", "root", req.Root, "err", err)
		return
	}
	upstream, err := net.Dial("unix", socket)
	if err != nil {
		s.log.Warn("connecting to the code service", "err", err)
		return
	}
	defer upstream.Close()
	done := make(chan struct{}, 2)
	go func() { io.Copy(upstream, br); done <- struct{}{} }()
	go func() { io.Copy(conn, upstream); done <- struct{}{} }()
	<-done
}

func (s *Server) ensure(ctx context.Context, root string) (string, error) {
	u, err := user.Lookup(s.user)
	if err != nil {
		return "", err
	}
	if root == "" || !path.IsAbs(root) {
		root = u.HomeDir
	}
	root = path.Clean(root)

	s.mu.Lock()
	defer s.mu.Unlock()
	if p := s.running[root]; p != nil {
		select {
		case <-p.exited:
		default:
			return p.socket, nil
		}
	}
	p, err := s.start(ctx, u, root)
	if err != nil {
		return "", err
	}
	s.running[root] = p
	return p.socket, nil
}

func (s *Server) start(ctx context.Context, u *user.User, root string) (*process, error) {
	if err := vscode.MountEditor(); err != nil {
		return nil, err
	}
	node := filepath.Join(vscode.MountPoint, "node")
	svc := filepath.Join(vscode.MountPoint, script)
	if _, err := os.Stat(svc); err != nil {
		return nil, fmt.Errorf("the editor disk has no Code service (%s)", script)
	}
	uid, _ := strconv.Atoi(u.Uid)
	gid, _ := strconv.Atoi(u.Gid)
	if err := os.MkdirAll(socketDir, 0o700); err != nil {
		return nil, err
	}
	if err := os.Chown(socketDir, uid, gid); err != nil {
		return nil, err
	}
	sum := sha256.Sum256([]byte(root))
	socket := filepath.Join(socketDir, hex.EncodeToString(sum[:6])+".sock")
	_ = os.Remove(socket)

	logOut, err := os.OpenFile(logFile, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return nil, err
	}
	defer logOut.Close()

	cmd := exec.Command(node, svc, "--socket", socket, "--root", root)
	cmd.Dir = u.HomeDir
	cmd.Env = []string{
		"HOME=" + u.HomeDir,
		"SSH_AUTH_SOCK=" + sysuser.SSHAuthSock,
		"USER=" + u.Username,
		"LOGNAME=" + u.Username,
		"PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
		"LANG=C.UTF-8",
	}
	cmd.Stdout = logOut
	cmd.Stderr = logOut
	if err := sysuser.RunAs(cmd, u); err != nil {
		return nil, err
	}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("starting the code service: %w", err)
	}
	p := &process{socket: socket, exited: make(chan struct{})}
	go func() {
		err := cmd.Wait()
		s.log.Warn("the code service exited", "root", root, "err", err)
		close(p.exited)
	}()
	s.log.Info("code service started", "root", root, "pid", cmd.Process.Pid)

	for {
		if c, err := net.Dial("unix", socket); err == nil {
			c.Close()
			return p, nil
		}
		select {
		case <-p.exited:
			return nil, fmt.Errorf("the code service exited while starting; see %s", logFile)
		case <-ctx.Done():
			cmd.Process.Kill()
			return nil, fmt.Errorf("the code service did not start in %s; see %s", startTimeout, logFile)
		case <-time.After(100 * time.Millisecond):
		}
	}
}
