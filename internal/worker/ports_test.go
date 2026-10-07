package worker_test

import (
	"context"
	"crypto/tls"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/blob"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/server"
	"github.com/csnewman/hangar/internal/worker"
)

// An environment's own web servers are reached on <anything>-env<short> hosts:
// plain HTTP at its port 80 and HTTPS at its port 443, as asked. A private
// environment's send a browser to Hangar to sign in, and refuse other
// sites; a public one's let anyone through.
func TestPorts(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	srv, err := server.New(server.Config{DB: dbtest.Open(t), Blobs: blob.NewMemory(), ProfileBlobs: blob.NewMemory(), Files: t.TempDir(), BootstrapToken: token, PublicURL: "http://hangar.test", Log: quiet()})
	if err != nil {
		t.Fatal(err)
	}
	go srv.Run(ctx)
	hs := httptest.NewServer(srv.Handler())
	defer hs.Close()
	tlsServer := httptest.NewTLSServer(srv.Handler())
	defer tlsServer.Close()
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
		`{"image":"img","cpus":1,"memory_mib":512,"display":"none","gpu":"none","repos":[],"placement":{}}}`, &tmpl)
	var env api.Environment
	call(t, hs.URL, http.MethodPost, "/api/frontend/environments", `{"template_id":"`+tmpl.ID+`","name":"web"}`, &env)
	waitFor(t, hs.URL, env.ID, func(e *api.Environment) bool { return e != nil && e.Phase == api.PhaseRunning })

	host := "app-env" + env.ShortID + ".hangar.test"
	for h, want := range map[string]bool{
		host:                                      true,
		"env" + env.ShortID + ".hangar.test":      true,
		"a-b-env" + env.ShortID + ".hangar.test":  true,
		"code" + env.ShortID + ".hangar.test":     false,
		"appenv" + env.ShortID + ".hangar.test":   false,
		"-env" + env.ShortID + ".hangar.test":     false,
		"app-env" + env.ShortID + "x.hangar.test": false,
		"app-env" + env.ID + ".hangar.test":       false,
	} {
		if srv.EnvironmentHost(h) != want {
			t.Errorf("%s: taken for an environment's web servers %v, want %v", h, !want, want)
		}
	}

	// A browser on the environment's host, which follows nothing itself.
	client := &http.Client{CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
		Transport: &http.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}}}
	do := func(base, path string, header http.Header) *http.Response {
		t.Helper()
		req, _ := http.NewRequest(http.MethodGet, base+path, nil)
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
	seen := func(resp *http.Response) worker.SimulatedPortRequest {
		t.Helper()
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("status %d", resp.StatusCode)
		}
		var got worker.SimulatedPortRequest
		if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
			t.Fatal(err)
		}
		return got
	}
	navigate := http.Header{"Sec-Fetch-Mode": {"navigate"}, "Sec-Fetch-Site": {"none"}}

	// Private: opening a page goes to Hangar to sign in, and back.
	resp := do(hs.URL, "/page?q=1", navigate)
	resp.Body.Close()
	if resp.StatusCode != http.StatusSeeOther {
		t.Fatalf("an unsigned-in page: status %d", resp.StatusCode)
	}
	toHangar, _ := url.Parse(resp.Header.Get("Location"))
	if toHangar.Host != "hangar.test" || toHangar.Query().Get("to") != "http://"+host+"/page?q=1" {
		t.Fatalf("sent to %s", toHangar)
	}
	// Hangar, with its session, sends it back with a ticket.
	req, _ := http.NewRequest(http.MethodGet, hs.URL+toHangar.RequestURI(), nil)
	back, err := (&http.Client{Jar: browser.Jar, CheckRedirect: client.CheckRedirect}).Do(req)
	if err != nil {
		t.Fatal(err)
	}
	back.Body.Close()
	ticketURL, _ := url.Parse(back.Header.Get("Location"))
	if back.StatusCode != http.StatusSeeOther || ticketURL.Host != host {
		t.Fatalf("Hangar answered %d, to %s", back.StatusCode, ticketURL)
	}
	resp = do(hs.URL, ticketURL.RequestURI(), navigate)
	resp.Body.Close()
	var cookie *http.Cookie
	for _, c := range resp.Cookies() {
		if c.Name == "hangar_env" {
			cookie = c
		}
	}
	if resp.StatusCode != http.StatusSeeOther || resp.Header.Get("Location") != "/page?q=1" || cookie == nil || !cookie.HttpOnly {
		t.Fatalf("redeeming: status %d, to %q, cookie %v", resp.StatusCode, resp.Header.Get("Location"), cookie)
	}
	sent := cookie.Name + "=" + cookie.Value

	got := seen(do(hs.URL, "/page?q=1", http.Header{"Cookie": {sent + "; app=kept"}, "Sec-Fetch-Site": {"same-origin"}}))
	if got.Port != 80 || got.TLS || got.Host != host || got.Path != "/page?q=1" || got.Environment != env.ID {
		t.Errorf("plain HTTP reached %+v", got)
	}
	if c := got.Header.Get("Cookie"); strings.Contains(c, "hangar_env") || !strings.Contains(c, "app=kept") {
		t.Errorf("the environment was sent the cookies %q", c)
	}

	for name, h := range map[string]http.Header{
		"no cookie":              {"Sec-Fetch-Mode": {"cors"}},
		"another site's request": {"Cookie": {sent}, "Sec-Fetch-Site": {"same-site"}, "Sec-Fetch-Mode": {"cors"}},
		"another host's origin":  {"Cookie": {sent}, "Origin": {"http://other-env" + env.ShortID + ".hangar.test"}},
	} {
		resp := do(hs.URL, "/", h)
		resp.Body.Close()
		if resp.StatusCode < 400 {
			t.Errorf("%s: status %d", name, resp.StatusCode)
		}
	}

	// Public: anyone, over either.
	call(t, hs.URL, http.MethodPut, "/api/frontend/environments/"+env.ID+"/ports", `{"public":true}`, nil)
	if got := seen(do(hs.URL, "/", nil)); got.Port != 80 {
		t.Errorf("public, plain HTTP reached port %d", got.Port)
	}
	if got := seen(do(tlsServer.URL, "/secure", nil)); got.Port != 443 || !got.TLS || got.Path != "/secure" {
		t.Errorf("public, HTTPS reached %+v", got)
	}
}
