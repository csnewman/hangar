package clientapi_test

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

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
	profiles := profile.NewStore(d, nil)
	srv := httptest.NewServer(clientapi.New(clientapi.Config{
		Environments: em,
		Users:        um,
		Profiles:     profiles,
		SSH:          &clientapi.SSHGateway{Host: "hangar.example.com", Port: 2222, HostKey: "ssh-ed25519 AAAA"},
		HostSuffix:   ".hangar.example.com",
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
		if status != 200 || me["username"] != "alice" || ssh["port"] != 2222.0 ||
			me["environment_host_suffix"] != ".hangar.example.com" {
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
	if list, _ := body.([]any); status != 200 || len(list) != 1 || list[0].(map[string]any)["name"] != "dev" ||
		list[0].(map[string]any)["short_id"] != env.ShortID {
		t.Fatalf("environments: %d %v", status, body)
	}
	status, body = f.call("GET", "/api/v1/environments/"+env.ID, "", bearer)
	if e, _ := body.(map[string]any); status != 200 || e["ports_public"] != false {
		t.Fatalf("an environment: %d %v", status, body)
	}

	// Its web servers made public, and private again.
	status, body = f.call("PUT", "/api/v1/environments/"+env.ID+"/ports", `{"public":true}`, bearer)
	if e, _ := body.(map[string]any); status != 200 || e["ports_public"] != true {
		t.Fatalf("public: %d %v", status, body)
	}
	status, body = f.call("PUT", "/api/v1/environments/"+env.ID+"/ports", `{"public":false}`, bearer)
	if e, _ := body.(map[string]any); status != 200 || e["ports_public"] != false {
		t.Fatalf("private: %d %v", status, body)
	}
	if status, _ := f.call("PUT", "/api/v1/environments/00000000-0000-4000-8000-00000000abcd/ports", `{"public":true}`, bearer); status != 404 {
		t.Fatalf("a missing environment's ports: %d", status)
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

	// Reset takes an environment's settings back to its template's.
	if status, _ := f.call("POST", "/api/v1/environments/"+env.ID+"/stop", "", bearer); status != 200 {
		t.Fatalf("stop again: %d", status)
	}
	status, body = f.call("POST", "/api/v1/environments/"+env.ID+"/reset", "", bearer)
	if e, _ := body.(map[string]any); status != 200 || e["id"] != env.ID {
		t.Fatalf("reset: %d %v", status, body)
	}
	if status, _ := f.call("POST", "/api/v1/environments/00000000-0000-4000-8000-00000000abcd/reset", "", bearer); status != 404 {
		t.Fatalf("reset a missing environment: %d", status)
	}

	// Deleted, it goes, at once when it is not running, or once it stops.
	status, body = f.call("DELETE", "/api/v1/environments/"+env.ID, "", bearer)
	if e, _ := body.(map[string]any); status != 204 && (status != 200 || e["desired"] != "deleted") {
		t.Fatalf("delete: %d %v", status, body)
	}
	if status, _ := f.call("DELETE", "/api/v1/environments/00000000-0000-4000-8000-00000000abcd", "", bearer); status != 404 {
		t.Fatalf("delete a missing environment: %d", status)
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

// A task set and ended, and the agents' sessions seen, each reach an open
// event stream.
func TestTasksAndEvents(t *testing.T) {
	d := dbtest.Open(t)
	um := users.NewManager(d)
	em := environments.NewManager(d)
	changes := environments.NewChanges()
	ready := make(chan struct{})
	go d.Listen(t.Context(), func(channel, payload string) {
		if channel == "" {
			close(ready)
		}
		changes.Publish(payload)
	}, environments.Channel)
	<-ready
	srv := httptest.NewServer(clientapi.New(clientapi.Config{
		Environments: em,
		Users:        um,
		Profiles:     profile.NewStore(d, nil),
		Changes:      changes,
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
	bearer := "Authorization: Bearer " + token
	p := users.Principal{UserID: u.ID}
	tmpl, err := templates.NewManager(d).Create(ctx, p, templates.Input{
		Name: "t", Spec: api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024}},
	})
	if err != nil {
		t.Fatal(err)
	}
	env, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tmpl.ID, Name: "dev"})
	if err != nil {
		t.Fatal(err)
	}

	req, _ := http.NewRequestWithContext(t.Context(), "GET", srv.URL+"/api/v1/events", nil)
	req.Header.Set("Authorization", "Bearer "+token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != 200 || resp.Header.Get("Content-Type") != "text/event-stream" {
		t.Fatalf("events: %d %s", resp.StatusCode, resp.Header.Get("Content-Type"))
	}
	events := make(chan [2]string, 16)
	go func() {
		sc := bufio.NewScanner(resp.Body)
		var name string
		for sc.Scan() {
			line := sc.Text()
			if v, ok := strings.CutPrefix(line, "event: "); ok {
				name = v
			} else if v, ok := strings.CutPrefix(line, "data: "); ok {
				events <- [2]string{name, v}
			}
		}
		close(events)
	}()
	next := func(want string) map[string]any {
		t.Helper()
		select {
		case ev, ok := <-events:
			if !ok {
				t.Fatalf("waiting for %s: the stream ended", want)
			}
			if ev[0] != want {
				t.Fatalf("event %s %s, want %s", ev[0], ev[1], want)
			}
			var out map[string]any
			if err := json.Unmarshal([]byte(ev[1]), &out); err != nil {
				t.Fatal(err)
			}
			return out
		case <-time.After(5 * time.Second):
			t.Fatalf("no %s event", want)
		}
		return nil
	}
	if e := next("environment"); e["id"] != env.ID {
		t.Fatalf("the first event: %v", e)
	}
	next("synced")

	status, body := f.call("PUT", "/api/v1/environments/"+env.ID+"/task", `{"text":"Fix rounding"}`, bearer)
	if tk, _ := body.(map[string]any); status != 200 || tk["text"] != "Fix rounding" || tk["set_by"] != "user" {
		t.Fatalf("set a task: %d %v", status, body)
	}
	if e := next("environment"); e["task"].(map[string]any)["text"] != "Fix rounding" {
		t.Fatalf("after the task is set: %v", e)
	}
	if status, _ := f.call("PUT", "/api/v1/environments/"+env.ID+"/task", `{"text":"  "}`, bearer); status != 400 {
		t.Fatalf("an empty task: %d", status)
	}

	if err := em.ReportAgents(ctx, env.ID, []api.AgentSession{{ID: "s1", Agent: api.AgentClaude, State: api.AgentFinished,
		Since: time.Now().Add(-time.Second)}}); err != nil {
		t.Fatal(err)
	}
	e := next("environment")
	if a := e["agents"].([]any)[0].(map[string]any); a["state"] != "finished" || a["needs_you"] != true {
		t.Fatalf("a finished session: %v", a)
	}
	if status, _ := f.call("POST", "/api/v1/environments/"+env.ID+"/agents/s1/seen", "", bearer); status != 204 {
		t.Fatalf("seen: %d", status)
	}
	e = next("environment")
	if a := e["agents"].([]any)[0].(map[string]any); a["state"] != "idle" || a["needs_you"] != false {
		t.Fatalf("a seen session: %v", a)
	}
	if status, _ := f.call("POST", "/api/v1/environments/"+env.ID+"/agents/nope/seen", "", bearer); status != 404 {
		t.Fatalf("seeing no session: %d", status)
	}

	if status, _ := f.call("DELETE", "/api/v1/environments/"+env.ID+"/task", "", bearer); status != 204 {
		t.Fatalf("end the task: %d", status)
	}
	if e := next("environment"); e["task"] != nil {
		t.Fatalf("after the task is done: %v", e)
	}
	status, body = f.call("GET", "/api/v1/environments/"+env.ID+"/tasks", "", bearer)
	if list, _ := body.([]any); status != 200 || len(list) != 1 || list[0].(map[string]any)["done_at"] == nil {
		t.Fatalf("tasks: %d %v", status, body)
	}

	if _, err := em.SetDesired(ctx, p, env.ID, api.DesiredDeleted); err != nil {
		t.Fatal(err)
	}
	if e := next("removed"); e["id"] != env.ID {
		t.Fatalf("after it is deleted: %v", e)
	}
}
