package worker_test

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/cookiejar"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/dbtest"

	"github.com/csnewman/hangar/internal/server"
	"github.com/csnewman/hangar/internal/terminal"
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

	var tmpl struct{ ID string }
	call(t, hs.URL, http.MethodPost, "/api/frontend/templates", `{"name":"t","visibility":"private","spec":`+
		`{"image":"img","cpus":1,"memory_mib":512,"display":"none","gpu":"none","repos":[],"placement":{}}}`, &tmpl)
	var env api.Environment
	call(t, hs.URL, http.MethodPost, "/api/frontend/environments", `{"template_id":"`+tmpl.ID+`","name":"e2e"}`, &env)

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

// A terminal, end to end: browser WebSocket to the server, through the
// worker's tunnel, to a shell the runtime holds -- and a second window on
// the same session, which is what a refresh or another tab is.
func TestTerminal(t *testing.T) {
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

	w, err := worker.New(writeConfig(t, hs.URL), worker.NewSimulated(50*time.Millisecond), quiet())
	if err != nil {
		t.Fatal(err)
	}
	werr := make(chan error, 1)
	go func() { werr <- w.Run(ctx) }()
	// The worker stops before the server does: the server's Close waits for
	// the worker's held request for its desired set.
	defer func() {
		cancel()
		<-werr
	}()

	var tmpl struct{ ID string }
	call(t, hs.URL, http.MethodPost, "/api/frontend/templates", `{"name":"t","visibility":"private","spec":`+
		`{"image":"img","cpus":1,"memory_mib":512,"display":"none","gpu":"none","repos":[],"placement":{}}}`, &tmpl)
	var env api.Environment
	call(t, hs.URL, http.MethodPost, "/api/frontend/environments", `{"template_id":"`+tmpl.ID+`","name":"term"}`, &env)
	waitFor(t, hs.URL, env.ID, func(e *api.Environment) bool { return e != nil && e.Phase == api.PhaseRunning })

	u, _ := url.Parse(hs.URL)
	dial := func(query string) (*websocket.Conn, terminal.Reply) {
		t.Helper()
		ws := strings.Replace(hs.URL, "http", "ws", 1) + "/api/frontend/environments/" + env.ID + "/terminal?" + query
		var c *websocket.Conn
		var err error
		// The worker's tunnel may still be coming up.
		for range 50 {
			c, _, err = websocket.Dial(ctx, ws, &websocket.DialOptions{HTTPHeader: http.Header{
				"Cookie": {cookieHeader(browser.Jar.Cookies(u))},
			}})
			if err == nil {
				break
			}
			time.Sleep(100 * time.Millisecond)
		}
		if err != nil {
			t.Fatal(err)
		}
		c.SetReadLimit(-1)
		_, first, err := c.Read(ctx)
		if err != nil {
			t.Fatal(err)
		}
		var r terminal.Reply
		if err := json.Unmarshal(first, &r); err != nil || r.Err != "" {
			t.Fatalf("attach reply %s: %v", first, err)
		}
		return c, r
	}
	readUntil := func(c *websocket.Conn, want string) {
		t.Helper()
		rctx, rcancel := context.WithTimeout(ctx, 15*time.Second)
		defer rcancel()
		var seen strings.Builder
		for !strings.Contains(seen.String(), want) {
			_, msg, err := c.Read(rctx)
			if err != nil {
				t.Fatalf("waiting for %q, saw %q: %v", want, seen.String(), err)
			}
			if len(msg) > 0 && msg[0] == terminal.FrameOutput {
				seen.Write(msg[1:])
			}
		}
	}

	first, r := dial("cols=100&rows=30")
	session := r.Session.ID
	first.Write(ctx, websocket.MessageBinary, append([]byte{terminal.FrameInput}, "echo mark-$((40+2))\n"...))
	readUntil(first, "mark-42")

	// Another window on the same session sees what was already there.
	second, r := dial("session=" + session)
	if r.Session.Clients != 2 {
		t.Fatalf("second window: %d clients, want 2", r.Session.Clients)
	}
	readUntil(second, "mark-42")
	first.CloseNow()

	var sessions []struct {
		ID      string
		Clients int
	}
	call(t, hs.URL, http.MethodGet, "/api/frontend/environments/"+env.ID+"/terminals", "", &sessions)
	if len(sessions) != 1 || sessions[0].ID != session {
		t.Fatalf("sessions %+v", sessions)
	}
	call(t, hs.URL, http.MethodDelete, "/api/frontend/environments/"+env.ID+"/terminals/"+session, "", nil)
	rctx, rcancel := context.WithTimeout(ctx, 15*time.Second)
	defer rcancel()
	for exited := false; !exited; {
		_, msg, err := second.Read(rctx)
		if err != nil {
			t.Fatalf("the window was not told the session ended: %v", err)
		}
		exited = len(msg) > 0 && msg[0] == terminal.FrameExit
	}

	// Someone who may not reach the environment cannot open its terminal.
	if _, err := srv.Users().Create(ctx, users.NewUser{Username: "stranger", Password: "password1"}); err != nil {
		t.Fatal(err)
	}
	jar, _ := cookiejar.New(nil)
	stranger := &http.Client{Jar: jar}
	resp, err := stranger.Post(hs.URL+"/api/frontend/auth/login", "application/json",
		strings.NewReader(`{"username":"stranger","password":"password1"}`))
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	ws := strings.Replace(hs.URL, "http", "ws", 1) + "/api/frontend/environments/" + env.ID + "/terminal"
	_, resp, err = websocket.Dial(ctx, ws, &websocket.DialOptions{HTTPHeader: http.Header{
		"Cookie": {cookieHeader(jar.Cookies(u))},
	}})
	if err == nil || resp == nil || resp.StatusCode != http.StatusNotFound {
		t.Fatalf("a stranger opening the terminal: %v %v", resp, err)
	}
}

func cookieHeader(cs []*http.Cookie) string {
	parts := make([]string, len(cs))
	for i, c := range cs {
		parts[i] = c.Name + "=" + c.Value
	}
	return strings.Join(parts, "; ")
}
