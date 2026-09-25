package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"log/slog"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/csnewman/hangar/internal/vm"
)

// stateDir is where an environment's machine keeps what outlives a boot:
// its saved configuration, backend state, and a suspend's snapshot. It
// survives a host restart.
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

// controlSocket is where the process holding an environment answers other
// hangar processes about it.
func controlSocket(name string) string {
	return filepath.Join(os.TempDir(), "hangar-"+name+"-control.sock")
}

// holdMachine boots or resumes a machine, proves its agent answers, and
// holds it until the guest stops, it is suspended, or ctx ends -- or, given
// a probe, runs that and powers it off.
func holdMachine(ctx context.Context, cfg vm.InstanceConfig, agentWait time.Duration, probe *probeCmd) error {
	// Ctrl-C, or a SIGTERM, powers the guest off rather than orphaning it.
	ctx, stopSignals := signal.NotifyContext(ctx, os.Interrupt, syscall.SIGTERM)
	defer stopSignals()
	cfg.AgentWait = agentWait
	cfg.Log = slog.New(slog.NewTextHandler(os.Stderr, nil))
	cfg.Verbose = true
	if cfg.ConsoleFile == "" {
		// The console is the monitor's stdio, and so this terminal.
		cfg.Monitor, cfg.Stdin = os.Stdout, os.Stdin
	} else {
		cfg.Monitor = os.Stderr
	}
	inst, err := vm.Boot(ctx, cfg)
	if err != nil {
		return err
	}
	sess := inst.Session()
	fmt.Fprintf(os.Stderr, "\nagent       %s, kernel %s, guest up %.2fs\n",
		sess.Hello.Hostname, sess.Hello.Kernel, float64(sess.Hello.BootMicros)/1e6)

	// Prove the channel end to end rather than just that something
	// connected: a ping exercises the request path, and running a command
	// exercises the half an environment is actually for.
	if err := sess.Ping(5 * time.Second); err != nil {
		inst.Shutdown(cfg.Log)
		return fmt.Errorf("agent did not answer a ping: %w", err)
	}
	who, err := sess.Exec(10*time.Second, "id", "-un")
	if err != nil {
		inst.Shutdown(cfg.Log)
		return fmt.Errorf("agent could not run a command: %w", err)
	}
	fmt.Fprintf(os.Stderr, "agent       ping ok, exec ok (runs as %s)\n", strings.TrimSpace(who.Stdout))

	// A command to run inside the environment is a diagnostic: it reports
	// what the guest sees rather than what the console shows.
	if probe != nil {
		out, err := sess.Exec(probe.timeout, "sh", "-c", probe.cmd)
		if err == nil {
			fmt.Print(out.Stdout)
			fmt.Fprint(os.Stderr, out.Stderr)
		}
		inst.Shutdown(cfg.Log)
		return err
	}

	h := &holder{inst: inst, suspended: make(chan struct{})}
	stop, err := serveControl(ctx, controlSocket(cfg.ID), h)
	if err != nil {
		inst.Shutdown(cfg.Log)
		return err
	}
	// The control socket goes last of all, once the monitor and every
	// backend have stopped, so its disappearing tells a waiting suspend
	// that the machine has let go of everything.
	defer stop()

	select {
	case <-h.suspended:
	case <-inst.Exited():
		// A suspend stops the monitor too; that is not the guest stopping.
		h.mu.Lock()
		suspending := h.suspending
		h.mu.Unlock()
		if !suspending {
			inst.Close()
			return inst.Err()
		}
		<-h.suspended
	case <-ctx.Done():
		inst.Shutdown(cfg.Log)
		return nil
	}
	h.mu.Lock()
	err = h.suspendErr
	h.mu.Unlock()
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "suspended   %s -> %s\n", cfg.Name, cfg.Dir)
	return nil
}

// holder is the machine the process holds, and whether it is being
// suspended.
type holder struct {
	inst      *vm.Instance
	suspended chan struct{} // closed once a suspend has finished, and answered

	mu         sync.Mutex
	suspending bool
	suspendErr error
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

// serveControl answers requests from other hangar processes about the
// machine. They have to be answered by the process holding it: only it
// holds the backends and the agent's session.
func serveControl(ctx context.Context, path string, h *holder) (func(), error) {
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
			go answer(ctx, conn, h)
		}
	}()
	return func() { ln.Close(); os.Remove(path) }, nil
}

func answer(ctx context.Context, conn net.Conn, h *holder) {
	defer conn.Close()
	line, err := bufio.NewReader(conn).ReadBytes('\n')
	if err != nil {
		return
	}
	var req controlRequest
	var reply controlReply
	done := false
	if err := json.Unmarshal(line, &req); err != nil {
		reply.Error = fmt.Sprintf("malformed request: %v", err)
	} else {
		switch req.Op {
		case "suspend":
			h.mu.Lock()
			already := h.suspending
			h.suspending = true
			h.mu.Unlock()
			if already {
				reply.Error = "already being suspended"
				break
			}
			start := time.Now()
			if err := h.inst.Suspend(ctx); err != nil {
				// The guest runs on.
				reply.Error = err.Error()
				h.mu.Lock()
				h.suspending = false
				h.mu.Unlock()
			} else {
				fmt.Fprintf(os.Stderr, "suspended in %.2fs\n", time.Since(start).Seconds())
				done = true
			}
		case "exec":
			timeout := time.Duration(req.TimeoutMS) * time.Millisecond
			if timeout <= 0 {
				timeout = 2 * time.Minute
			}
			resp, err := h.inst.Session().Exec(timeout, req.Cmd...)
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
	if done {
		// Answered first: the process ends once this is closed.
		close(h.suspended)
	}
}

// control sends one request to the process running an environment.
func control(name string, req controlRequest, wait time.Duration) (*controlReply, error) {
	conn, err := net.Dial("unix", controlSocket(name))
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
	sock := controlSocket(*name)
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
