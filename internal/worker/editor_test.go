package worker_test

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"net/url"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/editor"
	"github.com/csnewman/hangar/internal/publichost"
	"github.com/csnewman/hangar/internal/server"
	"github.com/csnewman/hangar/internal/worker"
)

// An environment's editor is served on a host of its own, named after
// Hangar's in either style.
func TestEditor(t *testing.T) {
	t.Run("subdomain", func(t *testing.T) {
		testEditor(t, publichost.Subdomain, "http://hangar.test", func(label string) string { return label + ".hangar.test" })
	})
	t.Run("prefix", func(t *testing.T) {
		testEditor(t, publichost.Prefix, "http://hangar1.corp.test", func(label string) string { return label + "-hangar1.corp.test" })
	})
}

func testEditor(t *testing.T, style publichost.Style, public string, name func(label string) string) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	srv, err := server.New(server.Config{DB: dbtest.Open(t), BootstrapToken: token, PublicURL: public, HostStyle: style,
		Log: quiet()})
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
	defer func() {
		cancel()
		<-werr
	}()

	var tmpl struct{ ID string }
	call(t, hs.URL, http.MethodPost, "/api/frontend/templates", `{"name":"t","visibility":"private","spec":`+
		`{"image":"img","cpus":1,"memory_mib":512,"display":"none","gpu":"none","repos":[],"placement":{},"editor_path":"/workspace/app"}}`, &tmpl)
	var env api.Environment
	call(t, hs.URL, http.MethodPost, "/api/frontend/environments", `{"template_id":"`+tmpl.ID+`","name":"ed"}`, &env)
	waitFor(t, hs.URL, env.ID, func(e *api.Environment) bool { return e != nil && e.Phase == api.PhaseRunning })

	var opened struct{ URL string }
	call(t, hs.URL, http.MethodPost, "/api/frontend/environments/"+env.ID+"/editor", "", &opened)
	signInURL, err := url.Parse(opened.URL)
	if err != nil {
		t.Fatal(err)
	}
	host := name("e-" + env.ID)
	if signInURL.Host != host {
		t.Fatalf("editor host %q, want %q", signInURL.Host, host)
	}

	// Requests go to the test server under the editor's host name, as a
	// browser's would through DNS.
	client := &http.Client{CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	do := func(path string, header http.Header) *http.Response {
		t.Helper()
		req, _ := http.NewRequest(http.MethodGet, hs.URL+path, nil)
		req.Host = host
		for k, v := range header {
			req.Header[k] = v
		}
		resp, err := client.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		return resp
	}

	resp := do(signInURL.RequestURI(), http.Header{"Sec-Fetch-Site": {"same-site"}, "Sec-Fetch-Mode": {"navigate"}})
	resp.Body.Close()
	if resp.StatusCode != http.StatusSeeOther || resp.Header.Get("Location") != "/?folder=%2Fworkspace%2Fapp" {
		t.Fatalf("sign-in: %d to %q", resp.StatusCode, resp.Header.Get("Location"))
	}
	var cookie *http.Cookie
	for _, c := range resp.Cookies() {
		if c.Name == editor.CookieName {
			cookie = c
		}
	}
	if cookie == nil || !cookie.HttpOnly || cookie.SameSite != http.SameSiteLaxMode {
		t.Fatalf("sign-in set %v", resp.Header.Values("Set-Cookie"))
	}
	resp = do(signInURL.RequestURI(), nil)
	resp.Body.Close()
	if resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("a ticket redeemed twice: %d", resp.StatusCode)
	}

	sent := cookie.Name + "=" + cookie.Value
	withCookie := http.Header{"Cookie": {sent + "; other=kept"}, "Sec-Fetch-Site": {"same-origin"}}
	var seen worker.SimulatedRequest
	// The worker's tunnel may still be coming up.
	for i := 0; ; i++ {
		resp = do("/_simulated/request", withCookie)
		if resp.StatusCode == http.StatusOK || i == 50 {
			break
		}
		resp.Body.Close()
		time.Sleep(100 * time.Millisecond)
	}
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("editor request: %d", resp.StatusCode)
	}
	json.NewDecoder(resp.Body).Decode(&seen)
	resp.Body.Close()
	if seen.Environment != env.ID || seen.Host != host {
		t.Errorf("the editor saw environment %q host %q", seen.Environment, seen.Host)
	}
	if got := seen.Header.Get("Cookie"); got != "other=kept" {
		t.Errorf("the editor was sent cookies %q; its own must not reach the guest", got)
	}
	if csp := resp.Header.Get("Content-Security-Policy"); csp != "frame-ancestors 'self' "+public {
		t.Errorf("framing policy %q", csp)
	}

	refused := map[string]http.Header{
		"no cookie":                  {"Sec-Fetch-Site": {"same-origin"}},
		"another environment":        {"Cookie": {sent}, "Sec-Fetch-Site": {"same-site"}, "Sec-Fetch-Mode": {"cors"}},
		"another site":               {"Cookie": {sent}, "Sec-Fetch-Site": {"cross-site"}, "Sec-Fetch-Mode": {"navigate"}},
		"a WebSocket from elsewhere": {"Cookie": {sent}, "Origin": {"http://" + name("e-00000000-0000-0000-0000-000000000000")}},
	}
	for name, h := range refused {
		resp = do("/_simulated/request", h)
		resp.Body.Close()
		if resp.StatusCode != http.StatusUnauthorized && resp.StatusCode != http.StatusForbidden {
			t.Errorf("%s: %d, want refused", name, resp.StatusCode)
		}
	}

	// Signing out of Hangar signs out of its editors.
	call(t, hs.URL, http.MethodPost, "/api/frontend/auth/logout", "", nil)
	resp = do("/_simulated/request", withCookie)
	resp.Body.Close()
	if resp.StatusCode != http.StatusUnauthorized {
		t.Errorf("after signing out of Hangar: %d", resp.StatusCode)
	}
}
