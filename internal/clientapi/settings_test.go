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
	"github.com/csnewman/hangar/internal/packs"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
	"github.com/csnewman/hangar/internal/workers"
)

// An environment's settings: its machine, what its template gives it, the
// copies of its packs, and its image.
func TestEnvironmentSettings(t *testing.T) {
	d := dbtest.Open(t)
	um := users.NewManager(d)
	em := environments.NewManager(d)
	packStore, err := packs.NewStore(d, t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(clientapi.New(clientapi.Config{
		Environments: em,
		Users:        um,
		Profiles:     profile.NewStore(d, nil),
		Packs:        packStore,
		Workers:      workers.NewManager(d),
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
	alice := users.Principal{UserID: u.ID}
	tmpl, err := templates.NewManager(d).Create(ctx, alice, templates.Input{
		Name: "t", Spec: api.TemplateSpec{Spec: api.Spec{
			Image: "img", CPUs: 1, MemoryMiB: 1024,
			Access:    api.Access{NoSSHKeys: true, NoRegistry: true},
			Placement: map[string]string{"runtime": "krun"},
		}},
	})
	if err != nil {
		t.Fatal(err)
	}
	env, err := em.Create(ctx, alice, api.CreateEnvironment{TemplateID: tmpl.ID, Name: "dev"})
	if err != nil {
		t.Fatal(err)
	}
	path := "/api/v1/environments/" + env.ID
	const missing = "/api/v1/environments/00000000-0000-4000-8000-00000000abcd"

	// What its template gives it, and keeps from it.
	status, body := f.call("GET", path, "", bearer)
	e, _ := body.(map[string]any)
	access, _ := e["access"].(map[string]any)
	placement, _ := e["placement"].(map[string]any)
	if status != 200 || access["no_ssh_keys"] != true || access["no_registry"] != true ||
		access["no_sensitive_files"] != nil || placement["runtime"] != "krun" || e["image"] != "img" ||
		e["template_id"] != tmpl.ID {
		t.Fatalf("its template's: %d %v", status, body)
	}

	// Not yet placed, its machine changes for its start.
	status, body = f.call("PUT", path+"/settings",
		`{"cpus":2,"memory_mib":2048,"display":"none","gpu":"none","swap_mib":0}`, bearer)
	e, _ = body.(map[string]any)
	if status != 200 || e["cpus"] != 2.0 || e["memory_mib"] != 2048.0 || e["swap_mib"] != 0.0 {
		t.Fatalf("settings: %d %v", status, body)
	}
	status, body = f.call("PUT", path+"/settings", `{"cpus":2,"memory_mib":2048,"display":"none","gpu":"none"}`, bearer)
	if e, _ := body.(map[string]any); status != 200 || e["swap_mib"] != nil {
		t.Fatalf("swap back to the worker's default: %d %v", status, body)
	}
	if status, _ := f.call("PUT", path+"/settings", `{"cpus":2,"memory_mib":2048,"display":"none","gpu":"sideways"}`, bearer); status != 400 {
		t.Fatalf("a GPU there is none of: %d", status)
	}
	if status, _ := f.call("PUT", missing+"/settings", `{"cpus":2,"memory_mib":2048,"display":"none","gpu":"none"}`, bearer); status != 404 {
		t.Fatalf("a missing environment's settings: %d", status)
	}

	// With no worker online, nothing is said of sizes.
	status, body = f.call("GET", "/api/v1/worker-sizes", "", bearer)
	if list, ok := body.([]any); status != 200 || !ok || len(list) != 0 {
		t.Fatalf("worker sizes: %d %v", status, body)
	}

	// Its packs: the profile, which attaches itself to everyone, in its
	// owner's own copy, which is among those to choose from.
	status, body = f.call("GET", path+"/packs", "", bearer)
	list, _ := body.([]any)
	if status != 200 || len(list) == 0 {
		t.Fatalf("packs: %d %v", status, body)
	}
	profilePack, _ := list[0].(map[string]any)
	copy, _ := profilePack["copy"].(map[string]any)
	copies, _ := profilePack["copies"].([]any)
	if profilePack["builtin"] != true || profilePack["listed"] != false || copy["personal"] != true || len(copies) == 0 {
		t.Fatalf("the profile: %v", profilePack)
	}
	pack := profilePack["id"].(string)
	if status, body := f.call("PUT", path+"/packs/"+pack, `{"copy_id":"`+copy["id"].(string)+`"}`, bearer); status != 204 {
		t.Fatalf("choosing a copy: %d %v", status, body)
	}
	status, body = f.call("GET", path+"/packs", "", bearer)
	if list, _ := body.([]any); status != 200 || list[0].(map[string]any)["from"] != "environment" {
		t.Fatalf("chosen: %d %v", status, body)
	}
	if status, _ := f.call("PUT", path+"/packs/"+pack, `{}`, bearer); status != 204 {
		t.Fatalf("the choice undone: %d", status)
	}
	if status, _ := f.call("PUT", path+"/packs/00000000-0000-4000-8000-00000000abcd", `{"copy_id":"`+copy["id"].(string)+`"}`, bearer); status != 400 {
		t.Fatalf("a copy not of the pack: %d", status)
	}
	if status, _ := f.call("GET", missing+"/packs", "", bearer); status != 404 {
		t.Fatalf("a missing environment's packs: %d", status)
	}

	// Its image: nothing newer to upgrade to, nothing kept to roll back to.
	if status, _ := f.call("POST", path+"/image/upgrade", `{}`, bearer); status != 409 {
		t.Fatalf("an upgrade to nothing: %d", status)
	}
	if status, _ := f.call("POST", path+"/image/rollback", "", bearer); status != 409 {
		t.Fatalf("a rollback to nothing: %d", status)
	}
	if status, _ := f.call("POST", path+"/image/sideways", "", bearer); status != 400 {
		t.Fatalf("a change there is none of: %d", status)
	}
	if status, _ := f.call("POST", missing+"/image/keep", "", bearer); status != 404 {
		t.Fatalf("a missing environment's image: %d", status)
	}
}
