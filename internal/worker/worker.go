// Package worker is hangar-worker: it holds the environments the control
// plane places on this machine.
//
// It is level-triggered. The server sends the complete set of environments
// this worker should hold, and the worker applies all of it every time,
// whether or not anything changed; it reports the complete set it actually
// holds, on every change and on a timer. A lost message on either side is
// repaired by the next one, so nothing needs acknowledging or replaying.
//
// Losing the server is not a reason to change anything. A worker that cannot
// reach it keeps running what it is running, and an environment the server
// has no record of is reported, never stopped.
package worker

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/tunnel"
)

// Runtime is what actually runs environments on a machine.
type Runtime interface {
	// Apply moves one environment towards spec.Desired. It returns promptly
	// and does the work in the background, and it is called again with the
	// same spec on every desired set, so it must be idempotent.
	Apply(spec api.EnvironmentSpec)
	// Observe returns every environment the runtime holds, whether or not
	// the server asked for it. An environment that has been deleted and
	// fully removed is absent.
	Observe() []api.ObservedEnvironment
	// Changed receives whenever Observe's answer may have changed.
	Changed() <-chan struct{}
}

// ImageStore is a runtime that keeps images in a local store of its own,
// which the control plane can see into and clear.
type ImageStore interface {
	// Images returns every image in the store.
	Images() []api.LocalImage
	// RemoveImage deletes an image from the store, unless an environment
	// uses it. It is asked again on every desired set until the image is
	// gone, so it must be idempotent.
	RemoveImage(ref string)
}

// TerminalDialer is a runtime whose environments have terminals. The
// control plane reaches one through the worker's tunnel; the runtime opens
// the connection to wherever the environment's sessions live, which speaks
// the internal/terminal protocol.
type TerminalDialer interface {
	DialTerminal(ctx context.Context, environment string) (net.Conn, error)
}

// CodeDialer is a runtime whose environments serve the Code tab.
type CodeDialer interface {
	DialCode(ctx context.Context, environment string) (net.Conn, error)
}

// DesktopDialer is a runtime whose environments may have a desktop. Each
// connection it opens carries one VNC (RFB) connection to it.
type DesktopDialer interface {
	DialDesktop(ctx context.Context, environment string) (net.Conn, error)
}

// EditorDialer is a runtime whose environments have an editor. Each
// connection it opens carries one HTTP connection to it.
type EditorDialer interface {
	DialEditor(ctx context.Context, environment string) (net.Conn, error)
}

// reportEvery is how often the worker reports even when nothing changed. The
// report carries usage figures, which change all the time, so this is also
// how fresh they are; it is well inside the server's online window, so a few
// lost reports do not make the worker look offline.
const reportEvery = 5 * time.Second

type Worker struct {
	cfg      *Config
	rt       Runtime
	client   *client
	capacity api.Resources
	log      *slog.Logger
	sys      *sysStats
}

// New creates a worker. The capacity it offers is this machine's, less what
// the configuration reserves for the host.
func New(cfg *Config, rt Runtime, log *slog.Logger) (*Worker, error) {
	total, err := machineCapacity()
	if err != nil {
		return nil, fmt.Errorf("measuring this machine: %w", err)
	}
	offered := api.Resources{
		CPUs:      max(total.CPUs-cfg.Reserved.CPUs, 0),
		MemoryMiB: max(total.MemoryMiB-cfg.Reserved.Memory.MiB(), 0),
	}
	return &Worker{cfg: cfg, rt: rt, client: newClient(cfg.Server.URL), capacity: offered, log: log,
		sys: &sysStats{path: cfg.Storage.Environments}}, nil
}

// Run registers if need be, then holds the desired set until ctx ends or the
// server rejects this worker's credential.
func (w *Worker) Run(ctx context.Context) error {
	if err := w.retry(ctx, "registering", func() error { return w.client.loadOrRegister(ctx, w.cfg) }); err != nil {
		return err
	}
	w.log.Info("worker ready", "name", w.cfg.Node.Name, "server", w.cfg.Server.URL,
		"cpus", w.capacity.CPUs, "memory_mib", w.capacity.MemoryMiB)

	ctx, cancel := context.WithCancelCause(ctx)
	defer cancel(nil)
	go func() {
		if err := w.reportLoop(ctx); err != nil {
			cancel(err)
		}
	}()
	go func() {
		if err := tunnel.Serve(ctx, w.cfg.Server.URL, w.client.credential, w.stream, w.log); err != nil {
			cancel(ErrRejected)
		}
	}()
	err := w.desiredLoop(ctx)
	if cause := context.Cause(ctx); cause != nil && !errors.Is(cause, context.Canceled) {
		return cause
	}
	return err
}

func (w *Worker) desiredLoop(ctx context.Context) error {
	var version int64
	for {
		var set api.DesiredSet
		err := w.retry(ctx, "fetching the desired set", func() error {
			var err error
			set, err = w.client.desired(ctx, version)
			return err
		})
		if err != nil {
			return err
		}
		if set.Version != version {
			w.log.Debug("desired set", "version", set.Version, "environments", len(set.Environments))
		}
		version = set.Version
		for _, spec := range set.Environments {
			w.rt.Apply(spec)
		}
		if store, ok := w.rt.(ImageStore); ok {
			for _, ref := range set.RemoveImages {
				store.RemoveImage(ref)
			}
		}
	}
}

func (w *Worker) reportLoop(ctx context.Context) error {
	t := time.NewTicker(reportEvery)
	defer t.Stop()
	for {
		err := w.retry(ctx, "reporting status", func() error {
			st := api.WorkerStatus{
				Capacity:     w.capacity,
				Labels:       w.cfg.Node.Labels,
				Environments: w.rt.Observe(),
				Stats:        w.sys.measure(),
				Images:       []api.LocalImage{},
			}
			if store, ok := w.rt.(ImageStore); ok {
				st.Images = store.Images()
			}
			return w.client.report(ctx, st)
		})
		if err != nil {
			return err
		}
		select {
		case <-ctx.Done():
			return nil
		case <-t.C:
		case <-w.rt.Changed():
			// Changes tend to arrive in bursts, and one report can carry
			// them all.
			select {
			case <-ctx.Done():
				return nil
			case <-time.After(100 * time.Millisecond):
			}
		}
	}
}

// retry runs fn until it succeeds, backing off between failures. A rejected
// credential ends it: no amount of retrying fixes a revoked worker.
func (w *Worker) retry(ctx context.Context, what string, fn func() error) error {
	backoff := 500 * time.Millisecond
	for {
		err := fn()
		if err == nil || errors.Is(err, ErrRejected) {
			return err
		}
		if ctx.Err() != nil {
			return ctx.Err()
		}
		w.log.Warn(what+" failed, retrying", "err", err, "after", backoff)
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(backoff):
		}
		backoff = min(backoff*2, 15*time.Second)
	}
}

// stream serves one stream the control plane opens over the tunnel.
func (w *Worker) stream(h tunnel.Header, r io.Reader, stream net.Conn) {
	defer stream.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	var guest net.Conn
	var err error
	switch h.Kind {
	case tunnel.KindTerminal:
		dialer, ok := w.rt.(TerminalDialer)
		if !ok {
			cancel()
			return
		}
		guest, err = dialer.DialTerminal(ctx, h.Environment)
	case tunnel.KindEditor:
		dialer, ok := w.rt.(EditorDialer)
		if !ok {
			cancel()
			// The stream carries HTTP, so the refusal is an HTTP response
			// the browser can show.
			io.WriteString(stream, "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nThis worker's environments have no editor.\n")
			return
		}
		guest, err = dialer.DialEditor(ctx, h.Environment)
	case tunnel.KindCode:
		dialer, ok := w.rt.(CodeDialer)
		if !ok {
			cancel()
			return
		}
		guest, err = dialer.DialCode(ctx, h.Environment)
	case tunnel.KindDesktop:
		dialer, ok := w.rt.(DesktopDialer)
		if !ok {
			cancel()
			return
		}
		guest, err = dialer.DialDesktop(ctx, h.Environment)
	default:
		cancel()
		w.log.Warn("unknown stream from the control plane", "kind", h.Kind)
		return
	}
	cancel()
	if err != nil {
		w.log.Warn("opening a stream to an environment", "kind", h.Kind, "environment", h.Environment, "err", err)
		return
	}
	defer guest.Close()
	// The bytes are relayed as they are; the protocol is the guest's and
	// the browser's, and nothing here needs to read it.
	done := make(chan struct{}, 2)
	go func() { io.Copy(guest, r); done <- struct{}{} }()
	go func() { io.Copy(stream, guest); done <- struct{}{} }()
	<-done
}
