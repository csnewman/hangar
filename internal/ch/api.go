package ch

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"time"
)

// API drives a running monitor through its API socket.
//
// It speaks the monitor's HTTP API directly rather than shelling out to
// ch-remote, so suspending an environment needs nothing installed beyond the
// monitor itself.
type API struct {
	socket string
	client *http.Client
}

// NewAPI returns a client for the monitor listening on socket.
func NewAPI(socket string) *API {
	return &API{
		socket: socket,
		client: &http.Client{
			Transport: &http.Transport{
				DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
					var d net.Dialer
					return d.DialContext(ctx, "unix", socket)
				},
			},
		},
	}
}

// WaitReady waits for the monitor to accept connections on its API socket.
func (a *API) WaitReady(ctx context.Context, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		// The socket file exists from the moment the monitor binds it, which
		// is before it listens; only a connection that is accepted shows it
		// is ready for requests.
		if c, err := net.DialTimeout("unix", a.socket, time.Second); err == nil {
			c.Close()
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(20 * time.Millisecond):
		}
	}
	return fmt.Errorf("the monitor did not accept connections on %s within %s", a.socket, timeout)
}

func (a *API) put(ctx context.Context, action string, body any) error {
	var r io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
		r = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPut, "http://localhost/api/v1/"+action, r)
	if err != nil {
		return err
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := a.client.Do(req)
	if err != nil {
		return fmt.Errorf("%s: %w", action, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode/100 != 2 {
		msg, _ := io.ReadAll(resp.Body)
		return fmt.Errorf("%s: %s: %s", action, resp.Status, bytes.TrimSpace(msg))
	}
	return nil
}

// Pause stops the guest's vCPUs and its devices' guest traffic.
func (a *API) Pause(ctx context.Context) error { return a.put(ctx, "vm.pause", nil) }

// Resume starts a paused guest again.
func (a *API) Resume(ctx context.Context) error { return a.put(ctx, "vm.resume", nil) }

// Snapshot writes a paused guest's memory and device state into dir.
func (a *API) Snapshot(ctx context.Context, dir string) error {
	return a.put(ctx, "vm.snapshot", map[string]string{"destination_url": "file://" + dir})
}

// Restore rebuilds a guest from a snapshot in dir. It is left paused.
func (a *API) Restore(ctx context.Context, dir string) error {
	return a.put(ctx, "vm.restore", map[string]string{"source_url": "file://" + dir})
}

// Shutdown stops the monitor process.
func (a *API) Shutdown(ctx context.Context) error { return a.put(ctx, "vmm.shutdown", nil) }

// Memory is a running guest's memory as its monitor has it, in MiB.
type Memory struct {
	// Boot is what the guest booted with, and Max what it can be resized up
	// to; they are the same for a guest whose memory cannot be resized.
	Boot, Max int
	// Requested is what it was last asked to have, and Actual what it has:
	// the guest plugs memory in and gives it back in its own time, and may
	// not manage to give back all it is asked to.
	Requested, Actual int
}

// Resizable reports whether the guest's memory can be resized while it runs.
func (m Memory) Resizable() bool { return m.Max > m.Boot }

// Fit is the size nearest mb the guest can be resized to: between Boot and
// Max, in whole blocks above Boot.
func (m Memory) Fit(mb int) int {
	if !m.Resizable() {
		return m.Boot
	}
	return m.Boot + plugged(min(mb, m.Max)-m.Boot, m.Max-m.Boot)
}

// Memory reads the guest's memory.
func (a *API) Memory(ctx context.Context) (Memory, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, "http://localhost/api/v1/vm.info", nil)
	if err != nil {
		return Memory{}, err
	}
	resp, err := a.client.Do(req)
	if err != nil {
		return Memory{}, fmt.Errorf("vm.info: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode/100 != 2 {
		msg, _ := io.ReadAll(resp.Body)
		return Memory{}, fmt.Errorf("vm.info: %s: %s", resp.Status, bytes.TrimSpace(msg))
	}
	var info struct {
		Config struct {
			Memory struct {
				Size           int64  `json:"size"`
				HotplugSize    *int64 `json:"hotplug_size"`
				HotpluggedSize *int64 `json:"hotplugged_size"`
			} `json:"memory"`
		} `json:"config"`
		MemoryActualSize int64 `json:"memory_actual_size"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&info); err != nil {
		return Memory{}, fmt.Errorf("vm.info: %w", err)
	}
	mb := func(p *int64) int {
		if p == nil {
			return 0
		}
		return int(*p >> 20)
	}
	c := info.Config.Memory
	boot := int(c.Size >> 20)
	return Memory{
		Boot:      boot,
		Max:       boot + mb(c.HotplugSize),
		Requested: boot + mb(c.HotpluggedSize),
		Actual:    int(info.MemoryActualSize >> 20),
	}, nil
}

// ResizeMemory asks the guest to have mb MiB, which must be one Fit gives.
// It returns once asked; Memory says how far the guest has got.
func (a *API) ResizeMemory(ctx context.Context, mb int) error {
	return a.put(ctx, "vm.resize", map[string]int64{"desired_ram": int64(mb) << 20})
}
