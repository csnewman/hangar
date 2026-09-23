package worker_test

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/cookiejar"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/server"
	"github.com/csnewman/hangar/internal/users"
	"github.com/csnewman/hangar/internal/worker"
)

const token = "test-bootstrap-token"

// browser is the signed-in client the tests call the frontend API with.
var browser *http.Client

// signIn creates an administrator and signs browser in as them.
func signIn(t *testing.T, srv *server.Server, base string) {
	t.Helper()
	if _, err := srv.Users().Create(context.Background(),
		users.NewUser{Username: "admin", Password: "password1", Admin: true}); err != nil {
		t.Fatal(err)
	}
	jar, _ := cookiejar.New(nil)
	browser = &http.Client{Jar: jar}
	call(t, base, http.MethodPost, "/api/frontend/auth/login", `{"username":"admin","password":"password1"}`, nil)
}

// A real server, a real worker over HTTP, and the simulated runtime: an
// environment created through the public API is placed, started, stopped and
// deleted without anything but the two programs' own loops acting.
func TestEndToEnd(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	srv, err := server.New(server.Config{DB: dbtest.Open(t), BootstrapToken: token, Log: quiet()})
	if err != nil {
		t.Fatal(err)
	}
	go srv.Run(ctx)
	hs := httptest.NewServer(srv.Handler())
	defer hs.Close()
	signIn(t, srv, hs.URL)

	cfg := writeConfig(t, hs.URL)
	w, err := worker.New(cfg, worker.NewSimulated(50*time.Millisecond), quiet())
	if err != nil {
		t.Fatal(err)
	}
	werr := make(chan error, 1)
	go func() { werr <- w.Run(ctx) }()

	var env api.Environment
	call(t, hs.URL, http.MethodPost, "/api/frontend/environments",
		`{"name":"e2e","image":"img","cpus":1,"memory_mib":512}`, &env)

	waitFor(t, hs.URL, env.ID, func(e *api.Environment) bool { return e != nil && e.Phase == api.PhaseRunning })
	call(t, hs.URL, http.MethodPost, "/api/frontend/environments/"+env.ID+"/stop", "", nil)
	waitFor(t, hs.URL, env.ID, func(e *api.Environment) bool { return e != nil && e.Phase == api.PhaseStopped })
	call(t, hs.URL, http.MethodDelete, "/api/frontend/environments/"+env.ID, "", nil)
	waitFor(t, hs.URL, env.ID, func(e *api.Environment) bool { return e == nil })

	// The issued credential is kept for the worker's next run.
	if _, err := os.Stat(cfg.Server.CredentialFile); err != nil {
		t.Fatalf("credential not saved: %v", err)
	}
	cancel()
	if err := <-werr; err != nil && err != context.Canceled {
		t.Fatalf("worker: %v", err)
	}
}

func TestRevokedWorkerStops(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	srv, err := server.New(server.Config{DB: dbtest.Open(t), BootstrapToken: token, Log: quiet()})
	if err != nil {
		t.Fatal(err)
	}
	go srv.Run(ctx)
	hs := httptest.NewServer(srv.Handler())
	defer hs.Close()
	signIn(t, srv, hs.URL)

	cfg := writeConfig(t, hs.URL)
	w, err := worker.New(cfg, worker.NewSimulated(time.Millisecond), quiet())
	if err != nil {
		t.Fatal(err)
	}
	werr := make(chan error, 1)
	go func() { werr <- w.Run(ctx) }()

	var workers []api.Worker
	deadline := time.Now().Add(10 * time.Second)
	for len(workers) == 0 && time.Now().Before(deadline) {
		call(t, hs.URL, http.MethodGet, "/api/frontend/workers", "", &workers)
		time.Sleep(20 * time.Millisecond)
	}
	if len(workers) == 0 {
		t.Fatal("worker never registered")
	}
	call(t, hs.URL, http.MethodPost, "/api/frontend/workers/"+workers[0].ID+"/revoke", "", nil)

	select {
	case err := <-werr:
		if err != worker.ErrRejected {
			t.Fatalf("revoked worker exited with %v, want ErrRejected", err)
		}
	case <-time.After(5 * time.Second):
		// Revoking wakes the worker's held request, so the refusal is
		// immediate rather than at the next report.
		t.Fatal("revoked worker kept running")
	}
}

func writeConfig(t *testing.T, url string) *worker.Config {
	t.Helper()
	dir := t.TempDir()
	tokenFile := filepath.Join(dir, "token")
	if err := os.WriteFile(tokenFile, []byte(token+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(dir, "worker.yaml")
	yaml := "server:\n" +
		"  url: " + url + "\n" +
		"  token_file: " + tokenFile + "\n" +
		"  credential_file: " + filepath.Join(dir, "state", "credential") + "\n" +
		"node:\n  name: test-worker\n" +
		"runtime: simulated\n"
	if err := os.WriteFile(path, []byte(yaml), 0o600); err != nil {
		t.Fatal(err)
	}
	cfg, err := worker.LoadConfig(path)
	if err != nil {
		t.Fatal(err)
	}
	return cfg
}

// waitFor polls the public API until ok holds for the environment, which is
// nil once it has been removed.
func waitFor(t *testing.T, base, id string, ok func(*api.Environment) bool) {
	t.Helper()
	deadline := time.Now().Add(15 * time.Second)
	var last *api.Environment
	for time.Now().Before(deadline) {
		var list []api.Environment
		call(t, base, http.MethodGet, "/api/frontend/environments", "", &list)
		last = nil
		for i := range list {
			if list[i].ID == id {
				last = &list[i]
			}
		}
		if ok(last) {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("condition never held; last seen %+v", last)
}

func call(t *testing.T, base, method, path, body string, out any) {
	t.Helper()
	req, _ := http.NewRequest(method, base+path, strings.NewReader(body))
	if body != "" {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := browser.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(resp.Body)
	if resp.StatusCode >= 300 {
		t.Fatalf("%s %s: %s %s", method, path, resp.Status, b)
	}
	if out != nil {
		if err := json.Unmarshal(b, out); err != nil {
			t.Fatal(err)
		}
	}
}

func quiet() *slog.Logger { return slog.New(slog.NewTextHandler(io.Discard, nil)) }
