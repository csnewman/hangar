package clientapi_test

import (
	"io"
	"log/slog"
	"net/http/httptest"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/clientapi"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
)

// The templates the user may use, and an environment made from one: as
// the template gives it, or with its own machine and repositories.
func TestCreateFromTemplate(t *testing.T) {
	d := dbtest.Open(t)
	um := users.NewManager(d)
	tm := templates.NewManager(d)
	srv := httptest.NewServer(clientapi.New(clientapi.Config{
		Environments: environments.NewManager(d),
		Users:        um,
		Profiles:     profile.NewStore(d, nil),
		Templates:    tm,
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
	tmpl, err := tm.Create(ctx, users.Principal{UserID: u.ID}, templates.Input{
		Name: "Rust service", Description: "A service and its tests",
		Spec: api.TemplateSpec{
			Spec: api.Spec{Image: "img", CPUs: 2, MemoryMiB: 4096, Display: api.DisplayNone,
				Repos: []api.Repo{{URL: "https://example.com/svc.git", Path: "/workspace/svc", Branch: "feat/{name}"}}},
			NamePattern: "[a-z]+-[0-9]+", NameHint: "a word and a number, such as rate-1",
		},
	})
	if err != nil {
		t.Fatal(err)
	}

	status, body := f.call("GET", "/api/v1/templates", "", bearer)
	list, _ := body.([]any)
	if status != 200 || len(list) != 1 {
		t.Fatalf("templates: %d %v", status, body)
	}
	got := list[0].(map[string]any)
	repos, _ := got["repos"].([]any)
	if got["id"] != tmpl.ID || got["name"] != "Rust service" || got["owner"] != "alice" || got["cpus"] != 2.0 ||
		got["memory_mib"] != 4096.0 || got["name_hint"] != "a word and a number, such as rate-1" || len(repos) != 1 ||
		repos[0].(map[string]any)["branch"] != "feat/{name}" {
		t.Fatalf("the template: %v", got)
	}

	// As the template gives it.
	status, body = f.call("POST", "/api/v1/environments", `{"template_id":"`+tmpl.ID+`","name":"rate-1"}`, bearer)
	e, _ := body.(map[string]any)
	if status != 201 || e["name"] != "rate-1" || e["cpus"] != 2.0 || e["template_id"] != tmpl.ID {
		t.Fatalf("made: %d %v", status, body)
	}
	// With its own machine and branch.
	status, body = f.call("POST", "/api/v1/environments", `{"template_id":"`+tmpl.ID+`","name":"rate-2",`+
		`"settings":{"cpus":4,"memory_mib":8192,"display":"desktop","gpu":"none"},`+
		`"repos":[{"url":"https://example.com/svc.git","path":"/workspace/svc","branch":"fix/limits"}]}`, bearer)
	if e, _ := body.(map[string]any); status != 201 || e["cpus"] != 4.0 || e["memory_mib"] != 8192.0 ||
		e["display"] != "desktop" {
		t.Fatalf("made with its own machine: %d %v", status, body)
	}
	// A name the pattern refuses, and one taken.
	if status, _ := f.call("POST", "/api/v1/environments", `{"template_id":"`+tmpl.ID+`","name":"Rate"}`, bearer); status != 400 {
		t.Fatalf("a name the pattern refuses: %d", status)
	}
	if status, _ := f.call("POST", "/api/v1/environments", `{"template_id":"`+tmpl.ID+`","name":"rate-1"}`, bearer); status != 409 {
		t.Fatalf("a name taken: %d", status)
	}
}
