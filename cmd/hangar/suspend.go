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
	"strings"
	"time"

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
	Name      string `json:"name"`
	Virtiofs  string `json:"virtiofs,omitempty"`
	Dax       int    `json:"dax"`
	Net       bool   `json:"net"`
	GPU       bool   `json:"gpu"`
	GPUVenus  bool   `json:"gpu_venus"`
	GPUWindow int    `json:"gpu_window"`
	CID       uint32 `json:"cid"`
	Seccomp   string `json:"seccomp,omitempty"`
	Suspended bool   `json:"suspended"`
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
// environment. Suspending is the only one: it has to be done by the process
// holding the backends, since only it can reach them.
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
			line, _ := bufio.NewReader(conn).ReadString('\n')
			switch strings.TrimSpace(line) {
			case "suspend":
				if err := l.suspend(ctx); err != nil {
					fmt.Fprintf(conn, "error: %v\n", err)
				} else {
					fmt.Fprintln(conn, "ok")
					conn.Close()
					// With the state on disk the monitor has nothing left to
					// do, and stopping it ends the process that owns it.
					_ = l.api.Shutdown(ctx)
					l.stopVM()
					continue
				}
			default:
				fmt.Fprintf(conn, "error: unknown request %q\n", strings.TrimSpace(line))
			}
			conn.Close()
		}
	}()
	return func() { ln.Close(); os.Remove(path) }, nil
}

// suspendEnv asks the process running an environment to suspend it.
func suspendEnv(ctx context.Context, argv []string) error {
	fs := flag.NewFlagSet("suspend", flag.ExitOnError)
	name := fs.String("name", "hangar-env", "environment name")
	if err := fs.Parse(argv); err != nil {
		return err
	}
	conn, err := net.Dial("unix", runBase(*name)+"-control.sock")
	if err != nil {
		return fmt.Errorf("%s is not running here: %w", *name, err)
	}
	defer conn.Close()
	if _, err := fmt.Fprintln(conn, "suspend"); err != nil {
		return err
	}
	_ = conn.SetReadDeadline(time.Now().Add(5 * time.Minute))
	reply, err := bufio.NewReader(conn).ReadString('\n')
	if err != nil && !errors.Is(err, os.ErrDeadlineExceeded) && reply == "" {
		return fmt.Errorf("no answer from %s: %w", *name, err)
	}
	reply = strings.TrimSpace(reply)
	if reply != "ok" {
		return fmt.Errorf("%s", strings.TrimPrefix(reply, "error: "))
	}
	dir, _ := stateDir(*name)
	fmt.Fprintf(os.Stderr, "%s suspended to %s\n", *name, dir)
	return nil
}
