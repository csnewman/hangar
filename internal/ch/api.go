package ch

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
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

// WaitReady waits for the monitor to create its API socket.
func (a *API) WaitReady(ctx context.Context, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(a.socket); err == nil {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(20 * time.Millisecond):
		}
	}
	return fmt.Errorf("the monitor did not create %s within %s", a.socket, timeout)
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
