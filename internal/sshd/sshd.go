// Package sshd is the SSH server inside an environment, which Hangar's SSH
// gateway reaches it through: `ssh <environment>@<hangar> -p 2222`, and VS
// Code's Remote-SSH over the same.
//
// It is the guest agent's, on a vsock port only the host can reach, and the
// host reaches it only for the gateway, which has already signed the person
// in with their key. So it asks nothing of whoever connects: it serves
// shells, commands and port forwarding as the environment's user, as sshd
// would for them.
package sshd

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/binary"
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
	"syscall"
	"time"

	"github.com/creack/pty"
	"golang.org/x/crypto/ssh"

	"github.com/csnewman/hangar/internal/sysuser"
)

// Port is the vsock port the guest agent serves SSH on.
const Port uint32 = 8107

// Server serves SSH as one user.
type Server struct {
	// SFTP is the command the sftp subsystem runs, as the user, when the
	// image has no OpenSSH sftp-server. Empty refuses the subsystem there.
	SFTP string

	user   string
	log    *slog.Logger
	config *ssh.ServerConfig
}

// NewServer serves as the named user. Its host key is made afresh: the only
// client, the gateway, reaches it over a channel that is already the host's
// own, and checks nothing of it.
func NewServer(userName string, log *slog.Logger) (*Server, error) {
	_, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		return nil, err
	}
	signer, err := ssh.NewSignerFromKey(priv)
	if err != nil {
		return nil, err
	}
	cfg := &ssh.ServerConfig{NoClientAuth: true, ServerVersion: "SSH-2.0-hangar-agent"}
	cfg.AddHostKey(signer)
	return &Server{user: userName, log: log, config: cfg}, nil
}

// Serve holds one SSH connection until it closes.
func (s *Server) Serve(conn net.Conn) {
	defer conn.Close()
	sconn, chans, reqs, err := ssh.NewServerConn(conn, s.config)
	if err != nil {
		s.log.Warn("ssh: handshake", "err", err)
		return
	}
	defer sconn.Close()
	// Keepalives and the like want an answer; none is granted.
	go ssh.DiscardRequests(reqs)
	for nc := range chans {
		switch nc.ChannelType() {
		case "session":
			go s.session(nc)
		case "direct-tcpip":
			go s.forward(nc)
		default:
			nc.Reject(ssh.UnknownChannelType, "not served here")
		}
	}
}

// forward carries a local port forward (ssh -L) to where it asks, from
// inside the environment.
func (s *Server) forward(nc ssh.NewChannel) {
	var req struct {
		Host       string
		Port       uint32
		OriginHost string
		OriginPort uint32
	}
	if err := ssh.Unmarshal(nc.ExtraData(), &req); err != nil {
		nc.Reject(ssh.ConnectionFailed, "malformed request")
		return
	}
	target, err := net.DialTimeout("tcp", net.JoinHostPort(req.Host, strconv.Itoa(int(req.Port))), 10*time.Second)
	if err != nil {
		nc.Reject(ssh.ConnectionFailed, err.Error())
		return
	}
	ch, reqs, err := nc.Accept()
	if err != nil {
		target.Close()
		return
	}
	go ssh.DiscardRequests(reqs)
	pipe(ch, target)
}

func pipe(a io.ReadWriteCloser, b io.ReadWriteCloser) {
	var wg sync.WaitGroup
	wg.Add(2)
	go func() { io.Copy(a, b); a.Close(); wg.Done() }()
	go func() { io.Copy(b, a); b.Close(); wg.Done() }()
	wg.Wait()
}

type ptyRequest struct {
	Term     string
	Cols     uint32
	Rows     uint32
	Width    uint32
	Height   uint32
	Modelist string
}

// session serves one session channel: a shell, a command or a subsystem,
// with or without a terminal.
func (s *Server) session(nc ssh.NewChannel) {
	ch, reqs, err := nc.Accept()
	if err != nil {
		return
	}
	defer ch.Close()
	var env []string
	var term *ptyRequest
	var ptmx *os.File
	var started bool
	done := make(chan struct{})
	for req := range reqs {
		ok := false
		switch req.Type {
		case "env":
			var kv struct{ Name, Value string }
			if ssh.Unmarshal(req.Payload, &kv) == nil && !started {
				env = append(env, kv.Name+"="+kv.Value)
				ok = true
			}
		case "pty-req":
			var p ptyRequest
			if ssh.Unmarshal(req.Payload, &p) == nil && !started {
				term, ok = &p, true
			}
		case "window-change":
			var w struct{ Cols, Rows, Width, Height uint32 }
			if ssh.Unmarshal(req.Payload, &w) == nil && ptmx != nil {
				pty.Setsize(ptmx, &pty.Winsize{Cols: uint16(w.Cols), Rows: uint16(w.Rows)})
				ok = true
			}
		case "shell", "exec", "subsystem":
			if started {
				break
			}
			var command string
			switch req.Type {
			case "exec":
				var c struct{ Command string }
				if ssh.Unmarshal(req.Payload, &c) != nil {
					break
				}
				command = c.Command
			case "subsystem":
				var sub struct{ Name string }
				if ssh.Unmarshal(req.Payload, &sub) != nil || sub.Name != "sftp" {
					break
				}
				if command = sftpServer(); command == "" {
					command = s.SFTP
				}
			}
			if req.Type == "subsystem" && command == "" {
				break
			}
			cmd, err := s.command(command, env, term)
			if err != nil {
				fmt.Fprintf(ch.Stderr(), "%v\r\n", err)
				break
			}
			if ptmx, err = s.start(cmd, ch, term, done); err != nil {
				fmt.Fprintf(ch.Stderr(), "%v\r\n", err)
				break
			}
			started, ok = true, true
		case "signal":
			ok = true
		}
		if req.WantReply {
			req.Reply(ok, nil)
		}
	}
	if started {
		<-done
	}
}

// command is what a request runs: the user's login shell, or a command
// given to their shell, as sshd would.
func (s *Server) command(command string, env []string, term *ptyRequest) (*exec.Cmd, error) {
	u, err := user.Lookup(s.user)
	if err != nil {
		if u, err = user.Current(); err != nil {
			return nil, err
		}
	}
	shell := sysuser.LoginShell(u.Username)
	var cmd *exec.Cmd
	if command == "" {
		// argv[0] starting with "-" makes it a login shell.
		cmd = exec.Command(shell)
		cmd.Args = []string{"-" + filepath.Base(shell)}
	} else {
		cmd = exec.Command(shell, "-c", command)
	}
	cmd.Dir = u.HomeDir
	cmd.Env = append([]string{
		"HOME=" + u.HomeDir,
		"USER=" + u.Username,
		"LOGNAME=" + u.Username,
		"SHELL=" + shell,
		"SSH_AUTH_SOCK=" + sysuser.SSHAuthSock,
		"PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
		"LANG=C.UTF-8",
		"SSH_CONNECTION=127.0.0.1 0 127.0.0.1 22",
	}, env...)
	if term != nil {
		cmd.Env = append(cmd.Env, "TERM="+term.Term)
	}
	if err := sysuser.RunAs(cmd, u); err != nil {
		return nil, err
	}
	return cmd, nil
}

// start runs cmd on ch: on a terminal if one was asked for, on pipes
// otherwise. done closes once it has exited and its status is sent.
func (s *Server) start(cmd *exec.Cmd, ch ssh.Channel, term *ptyRequest, done chan<- struct{}) (*os.File, error) {
	finish := func(err error) {
		code := 0
		if err != nil {
			var exit *exec.ExitError
			if errors.As(err, &exit) {
				if st, ok := exit.Sys().(syscall.WaitStatus); ok && st.Signaled() {
					code = 128 + int(st.Signal())
				} else {
					code = exit.ExitCode()
				}
			} else {
				code = 255
			}
		}
		// The end of the output comes before the status, as sshd sends it.
		ch.CloseWrite()
		status := make([]byte, 4)
		binary.BigEndian.PutUint32(status, uint32(code))
		ch.SendRequest("exit-status", false, status)
		ch.Close()
		close(done)
	}
	if term != nil {
		ptmx, err := pty.StartWithSize(cmd, &pty.Winsize{Cols: uint16(term.Cols), Rows: uint16(term.Rows)})
		if err != nil {
			return nil, err
		}
		go io.Copy(ptmx, ch)
		go func() {
			io.Copy(ch, ptmx)
			err := cmd.Wait()
			ptmx.Close()
			finish(err)
		}()
		return ptmx, nil
	}
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, err
	}
	cmd.Stdout = ch
	cmd.Stderr = ch.Stderr()
	if err := cmd.Start(); err != nil {
		return nil, err
	}
	go func() {
		io.Copy(stdin, ch)
		stdin.Close()
	}()
	go func() { finish(cmd.Wait()) }()
	return nil, nil
}

// sftpServer is OpenSSH's SFTP server, where the image has one.
func sftpServer() string {
	for _, p := range []string{"/usr/lib/openssh/sftp-server", "/usr/libexec/openssh/sftp-server", "/usr/lib/sftp-server"} {
		if st, err := os.Stat(p); err == nil && !st.IsDir() {
			return p
		}
	}
	return ""
}

// FileConn makes a connected file, such as an accepted vsock connection, a
// net.Conn for the SSH library.
func FileConn(f *os.File) net.Conn { return fileConn{f} }

type fileConn struct{ *os.File }

func (fileConn) LocalAddr() net.Addr  { return addr{} }
func (fileConn) RemoteAddr() net.Addr { return addr{} }

type addr struct{}

func (addr) Network() string { return "vsock" }
func (addr) String() string  { return "host" }

// Pipe is a connected pair of sockets, for serving SSH to something in the
// same process. Unlike net.Pipe it is buffered: both ends of an SSH
// connection write their version before either reads.
func Pipe() (net.Conn, net.Conn, error) {
	fds, err := syscall.Socketpair(syscall.AF_UNIX, syscall.SOCK_STREAM, 0)
	if err != nil {
		return nil, nil, err
	}
	conns := make([]net.Conn, 2)
	for i, fd := range fds {
		f := os.NewFile(uintptr(fd), "sshd-pipe")
		c, err := net.FileConn(f)
		f.Close()
		if err != nil {
			return nil, nil, err
		}
		conns[i] = c
	}
	return conns[0], conns[1], nil
}
