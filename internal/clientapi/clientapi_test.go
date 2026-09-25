package clientapi_test

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/clientapi"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
)

var ctx = context.Background()

type fixture struct {
	t    *testing.T
	base string
}

// call makes a request with the given headers ("Name: value") and returns
// the status and the decoded body.
func (f fixture) call(method, path, body string, headers ...string) (int, any) {
	f.t.Helper()
	req, _ := http.NewRequest(method, f.base+path, strings.NewReader(body))
	if body != "" {
		req.Header.Set("Content-Type", "application/json")
	}
	for _, h := range headers {
		k, v, _ := strings.Cut(h, ": ")
		req.Header.Set(k, v)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		f.t.Fatal(err)
	}
	defer resp.Body.Close()
	var out any
	_ = json.NewDecoder(resp.Body).Decode(&out)
	return resp.StatusCode, out
}

func TestClientAPI(t *testing.T) {
	d := dbtest.Open(t)
	um := users.NewManager(d)
	em := environments.NewManager(d)
	srv := httptest.NewServer(clientapi.New(clientapi.Config{
		Environments: em,
		Users:        um,
		Profiles:     profile.NewStore(d, nil),
		SSH:          &clientapi.SSHGateway{Host: "hangar.example.com", Port: 2222, HostKey: "ssh-ed25519 AAAA"},
		Log:          slog.New(slog.NewTextHandler(io.Discard, nil)),
	}))
	t.Cleanup(srv.Close)
	f := fixture{t: t, base: srv.URL}

	u, err := um.Create(ctx, users.NewUser{Username: "alice", Password: "correct-horse"})
	if err != nil {
		t.Fatal(err)
	}
	token, _, err := um.CreateToken(ctx, u.ID, "desktop", 0)
	if err != nil {
		t.Fatal(err)
	}
	session, _, err := um.Login(ctx, "alice", "correct-horse")
	if err != nil {
		t.Fatal(err)
	}
	bearer := "Authorization: Bearer " + token
	cookie := "Cookie: " + clientapi.CookieName + "=" + session

	// The version needs no sign-in; everything else does.
	status, body := f.call("GET", "/api/v1/version", "")
	if v, _ := body.(map[string]any); status != 200 || v["versions"].([]any)[0] != "v1" || v["server"] == "" {
		t.Fatalf("version: %d %v", status, body)
	}
	if status, _ := f.call("GET", "/api/v1/me", ""); status != 401 {
		t.Fatalf("me with nobody signed in: %d", status)
	}
	if status, _ := f.call("GET", "/api/v1/me", "", "Authorization: Bearer hgr_nonsense"); status != 401 {
		t.Fatalf("me with a bad token: %d", status)
	}

	// A token and the web UI's cookie each sign alice in.
	for _, auth := range []string{bearer, cookie} {
		status, body := f.call("GET", "/api/v1/me", "", auth)
		me, _ := body.(map[string]any)
		ssh, _ := me["ssh"].(map[string]any)
		if status != 200 || me["username"] != "alice" || ssh["port"] != 2222.0 {
			t.Fatalf("me by %q: %d %v", auth[:12], status, body)
		}
	}

	// An environment, listed, fetched and started.
	tmpl, err := templates.NewManager(d).Create(ctx, users.Principal{UserID: u.ID}, templates.Input{
		Name: "t", Spec: api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024}},
	})
	if err != nil {
		t.Fatal(err)
	}
	env, err := em.Create(ctx, users.Principal{UserID: u.ID}, api.CreateEnvironment{TemplateID: tmpl.ID, Name: "dev"})
	if err != nil {
		t.Fatal(err)
	}
	status, body = f.call("GET", "/api/v1/environments", "", bearer)
	if list, _ := body.([]any); status != 200 || len(list) != 1 || list[0].(map[string]any)["name"] != "dev" {
		t.Fatalf("environments: %d %v", status, body)
	}
	if status, _ := f.call("GET", "/api/v1/environments/"+env.ID, "", bearer); status != 200 {
		t.Fatalf("an environment: %d", status)
	}
	if status, _ := f.call("GET", "/api/v1/environments/00000000-0000-4000-8000-00000000abcd", "", bearer); status != 404 {
		t.Fatalf("a missing environment: %d", status)
	}
	status, body = f.call("POST", "/api/v1/environments/"+env.ID+"/stop", "", bearer)
	if e, _ := body.(map[string]any); status != 200 || e["desired"] != "stopped" {
		t.Fatalf("stop: %d %v", status, body)
	}

	// The cookie carries a change only from no page, or this server's own.
	if status, _ := f.call("POST", "/api/v1/environments/"+env.ID+"/start", "", cookie,
		"Origin: https://evil.example"); status != 403 {
		t.Fatalf("a cross-site start with the cookie: %d", status)
	}
	status, body = f.call("POST", "/api/v1/environments/"+env.ID+"/start", "", cookie)
	if e, _ := body.(map[string]any); status != 200 || e["desired"] != "running" {
		t.Fatalf("a start with the cookie from no page: %d %v", status, body)
	}

	// Sign-in keys, added and listed.
	key := "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB0v2OP9Td7AjOQCzzislbj5sqwJ6Ks6aX9uDPWkXAPX laptop"
	if status, body := f.call("POST", "/api/v1/me/login-keys", `{"public_key":"`+key+`"}`, bearer); status != 201 {
		t.Fatalf("add a key: %d %v", status, body)
	}
	status, body = f.call("GET", "/api/v1/me/login-keys", "", bearer)
	if list, _ := body.([]any); status != 200 || len(list) != 1 || list[0].(map[string]any)["name"] != "laptop" {
		t.Fatalf("keys: %d %v", status, body)
	}
}
