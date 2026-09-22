package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/agent"
	"github.com/csnewman/hangar/internal/ch"
)

// envRecord is how an environment was started.
//
// A suspended environment is a snapshot of the guest plus the state its
// backends wrote, but the backends themselves are processes, and resuming
// has to start them again with the same arguments before the monitor can
// reconnect to them. This is those arguments. It lives in the state
// directory, which survives a host restart; the sockets live in the temporary
// directory, which does not need to, since resuming creates them afresh at
// the same paths the snapshot names.
type envRecord struct {
	Name     string `json:"name"`
	Virtiofs string `json:"virtiofs,omitempty"`
	Dax      int    `json:"dax"`
	Net      bool   `json:"net"`
	GPU      bool   `json:"gpu"`
	GPUVenus bool   `json:"gpu_venus"`
	// GPUVenusRestore carries Vulkan state across a suspend; see
	// ch.StartGpuBackend.
	GPUVenusRestore bool   `json:"gpu_venus_restore,omitempty"`
	GPUWindow       int    `json:"gpu_window"`
	CID             uint32 `json:"cid"`
	Seccomp         string `json:"seccomp,omitempty"`
	Suspended       bool   `json:"suspended"`
}

// stateDir is where an environment's suspended state is kept.
func stateDir(name string) (string, error) {
	base := os.Getenv("HANGAR_STATE_DIR")
	if base == "" {
		if x := os.Getenv("XDG_STATE_HOME"); x != "" {
			base = filepath.Join(x, "hangar", "envs")
		} else {
			home, err := os.UserHomeDir()
			if err != nil {
				return "", err
			}
			base = filepath.Join(home, ".local", "state", "hangar", "envs")
		}
	}
	return filepath.Join(base, name), nil
}

func (e *envRecord) save(dir string) error {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	b, err := json.MarshalIndent(e, "", "  ")
	if err != nil {
		return err
	}
	tmp := filepath.Join(dir, "env.json.tmp")
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, filepath.Join(dir, "env.json"))
}

func loadEnv(dir string) (*envRecord, error) {
	b, err := os.ReadFile(filepath.Join(dir, "env.json"))
	if err != nil {
		return nil, err
	}
	var e envRecord
	if err := json.Unmarshal(b, &e); err != nil {
		return nil, fmt.Errorf("reading %s: %w", filepath.Join(dir, "env.json"), err)
	}
	return &e, nil
}

// runBase is the prefix every socket of an environment shares.
func runBase(name string) string {
	return filepath.Join(os.TempDir(), "hangar-"+name)
}

// liveEnv is a running environment, held by the process that owns it.
type liveEnv struct {
	rec    *envRecord
	dir    string
	api    *ch.API
	fs     *ch.FsBackend
	gpu    *ch.GpuBackend
	stopVM context.CancelFunc

	mu   sync.Mutex
	sess *agent.Session
}

func (l *liveEnv) setSession(s *agent.Session) {
	l.mu.Lock()
	l.sess = s
	l.mu.Unlock()
}

func (l *liveEnv) session() *agent.Session {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.sess
}

// controlRequest is one request on the control socket, a JSON line.
type controlRequest struct {
	Op        string   `json:"op"`
	Cmd       []string `json:"cmd,omitempty"`
	TimeoutMS int64    `json:"timeout_ms,omitempty"`
}

// controlReply answers one request.
type controlReply struct {
	Error  string `json:"error,omitempty"`
	Stdout string `json:"stdout,omitempty"`
	Stderr string `json:"stderr,omitempty"`
	Code   int    `json:"code"`
}

// suspend writes the environment to disk and stops it.
//
// The order matters. The guest is paused first so nothing it holds changes
// while it is written down; each backend then records what the guest is
// holding, and only then is the snapshot taken, so the two describe the same
// instant.
func (l *liveEnv) suspend(ctx context.Context) error {
	start := time.Now()
	if err := l.api.Pause(ctx); err != nil {
		return err
	}
	if l.fs != nil {
		if err := l.fs.SaveState(30 * time.Second); err != nil {
			return err
		}
	}
	if l.gpu != nil {
		if err := l.gpu.SaveState(30 * time.Second); err != nil {
			return err
		}
	}
	snap := filepath.Join(l.dir, "snapshot")
	if err := os.RemoveAll(snap); err != nil {
		return err
	}
	if err := os.MkdirAll(snap, 0o755); err != nil {
		return err
	}
	if err := l.api.Snapshot(ctx, snap); err != nil {
		return err
	}
	l.rec.Suspended = true
	if err := l.rec.save(l.dir); err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "suspended   %s -> %s in %.2fs\n", l.rec.Name, l.dir, time.Since(start).Seconds())
	return nil
}

// serveControl answers requests from other hangar processes about this
// environment. They have to be answered by the process holding it: only it can
// reach the backends to suspend them, and only it holds the agent's session.
func serveControl(ctx context.Context, path string, l *liveEnv) (func(), error) {
	_ = os.Remove(path)
	ln, err := net.Listen("unix", path)
	if err != nil {
		return nil, fmt.Errorf("control socket: %w", err)
	}
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go l.answer(ctx, conn)
		}
	}()
	return func() { ln.Close(); os.Remove(path) }, nil
}

func (l *liveEnv) answer(ctx context.Context, conn net.Conn) {
	defer conn.Close()
	line, err := bufio.NewReader(conn).ReadBytes('\n')
	if err != nil {
		return
	}
	var req controlRequest
	var reply controlReply
	stop := false
	if err := json.Unmarshal(line, &req); err != nil {
		reply.Error = fmt.Sprintf("malformed request: %v", err)
	} else {
		switch req.Op {
		case "suspend":
			if err := l.suspend(ctx); err != nil {
				reply.Error = err.Error()
			} else {
				stop = true
			}
		case "exec":
			sess := l.session()
			if sess == nil {
				reply.Error = "the environment's agent is not connected"
				break
			}
			timeout := time.Duration(req.TimeoutMS) * time.Millisecond
			if timeout <= 0 {
				timeout = 2 * time.Minute
			}
			resp, err := sess.Exec(timeout, req.Cmd...)
			if err != nil {
				reply.Error = err.Error()
			} else {
				reply.Stdout, reply.Stderr, reply.Code = resp.Stdout, resp.Stderr, resp.Code
			}
		default:
			reply.Error = fmt.Sprintf("unknown request %q", req.Op)
		}
	}
	b, _ := json.Marshal(reply)
	_, _ = conn.Write(append(b, '\n'))
	if stop {
		// With the state on disk the monitor has nothing left to do, and
		// stopping it ends the process that owns it.
		_ = l.api.Shutdown(ctx)
		l.stopVM()
	}
}

// control sends one request to the process running an environment.
func control(name string, req controlRequest, wait time.Duration) (*controlReply, error) {
	conn, err := net.Dial("unix", runBase(name)+"-control.sock")
	if err != nil {
		return nil, fmt.Errorf("%s is not running here: %w", name, err)
	}
	defer conn.Close()
	b, err := json.Marshal(req)
	if err != nil {
		return nil, err
	}
	if _, err := conn.Write(append(b, '\n')); err != nil {
		return nil, err
	}
	_ = conn.SetReadDeadline(time.Now().Add(wait))
	line, err := bufio.NewReader(conn).ReadBytes('\n')
	if err != nil {
		return nil, fmt.Errorf("no answer from %s: %w", name, err)
	}
	var reply controlReply
	if err := json.Unmarshal(line, &reply); err != nil {
		return nil, fmt.Errorf("malformed answer from %s: %w", name, err)
	}
	if reply.Error != "" {
		return nil, errors.New(reply.Error)
	}
	return &reply, nil
}

// execEnv runs a command in a running environment.
func execEnv(argv []string) error {
	fs := flag.NewFlagSet("exec", flag.ExitOnError)
	name := fs.String("name", "hangar-env", "environment name")
	timeout := fs.Duration("timeout", 2*time.Minute, "how long to let the command run")
	if err := fs.Parse(argv); err != nil {
		return err
	}
	cmd := fs.Args()
	if len(cmd) == 0 {
		return fmt.Errorf("usage: hangar exec -name N -- command [args...]")
	}
	reply, err := control(*name, controlRequest{Op: "exec", Cmd: cmd, TimeoutMS: timeout.Milliseconds()}, *timeout+10*time.Second)
	if err != nil {
		return err
	}
	fmt.Print(reply.Stdout)
	fmt.Fprint(os.Stderr, reply.Stderr)
	if reply.Code != 0 {
		os.Exit(reply.Code)
	}
	return nil
}

// suspendEnv asks the process running an environment to suspend it.
func suspendEnv(ctx context.Context, argv []string) error {
	fs := flag.NewFlagSet("suspend", flag.ExitOnError)
	name := fs.String("name", "hangar-env", "environment name")
	if err := fs.Parse(argv); err != nil {
		return err
	}
	if _, err := control(*name, controlRequest{Op: "suspend"}, 5*time.Minute); err != nil {
		return err
	}
	// The state is on disk once the reply comes, but the environment is only
	// stopped when its process has let go of the monitor and backends, which
	// it marks by removing its control socket.
	sock := runBase(*name) + "-control.sock"
	deadline := time.Now().Add(time.Minute)
	for {
		if _, err := os.Stat(sock); os.IsNotExist(err) {
			break
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("%s was saved but has not stopped within a minute", *name)
		}
		time.Sleep(50 * time.Millisecond)
	}
	dir, _ := stateDir(*name)
	fmt.Fprintf(os.Stderr, "%s suspended to %s\n", *name, dir)
	return nil
}
