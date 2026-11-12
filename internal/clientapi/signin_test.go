package clientapi_test

import (
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/csnewman/hangar/internal/clientapi"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/users"
)

// An app signs in through the browser: it asks for a grant with no token,
// waits while it is not approved, collects a token once the person
// approves it (typing the code their own way), and cannot collect it
// twice. A refused grant says so.
func TestSignInThroughTheBrowser(t *testing.T) {
	d := dbtest.Open(t)
	um := users.NewManager(d)
	srv := httptest.NewServer(clientapi.New(clientapi.Config{
		Environments: environments.NewManager(d),
		Users:        um,
		Profiles:     profile.NewStore(d, nil),
		Log:          slog.New(slog.NewTextHandler(io.Discard, nil)),
	}))
	t.Cleanup(srv.Close)
	f := fixture{t: t, base: srv.URL}
	alice, err := um.Create(ctx, users.NewUser{Username: "alice", Password: "correct-horse"})
	if err != nil {
		t.Fatal(err)
	}

	if status, _ := f.call("POST", "/api/v1/sign-in", `{"name":""}`); status != http.StatusBadRequest {
		t.Fatalf("a nameless app: %d, want 400", status)
	}
	status, body := f.call("POST", "/api/v1/sign-in", `{"name":"Hangar Desktop on laptop"}`)
	if status != http.StatusCreated {
		t.Fatalf("starting: %d %v", status, body)
	}
	grant := body.(map[string]any)
	device, code := grant["device_code"].(string), grant["user_code"].(string)
	if len(code) != 9 || code[4] != '-' {
		t.Fatalf("code %q", code)
	}
	if want := "/connect/desktop?code=" + code; grant["verification_path"] != want {
		t.Fatalf("verification path %v, want %s", grant["verification_path"], want)
	}
	collect := func() (int, any) {
		return f.call("POST", "/api/v1/sign-in/token", `{"device_code":"`+device+`"}`)
	}
	if status, _ := collect(); status != http.StatusAccepted {
		t.Fatalf("before approval: %d, want 202", status)
	}
	if g, err := um.SignInGrant(ctx, code); err != nil || g.Name != "Hangar Desktop on laptop" {
		t.Fatalf("the grant as the page sees it: %+v %v", g, err)
	}
	typed := strings.ToLower(strings.ReplaceAll(code, "-", " "))
	if err := um.ApproveSignIn(ctx, typed, alice.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := um.SignInGrant(ctx, code); err == nil {
		t.Fatal("an approved grant still waiting")
	}
	status, body = collect()
	if status != http.StatusOK {
		t.Fatalf("collecting: %d %v", status, body)
	}
	token := body.(map[string]any)["token"].(string)
	status, body = f.call("GET", "/api/v1/me", "", "Authorization: Bearer "+token)
	if status != http.StatusOK || body.(map[string]any)["username"] != "alice" {
		t.Fatalf("the token signing in: %d %v", status, body)
	}
	if status, _ := collect(); status != http.StatusNotFound {
		t.Fatalf("collecting twice: %d, want 404", status)
	}

	_, body = f.call("POST", "/api/v1/sign-in", `{"name":"someone's script"}`)
	grant = body.(map[string]any)
	if err := um.RefuseSignIn(ctx, grant["user_code"].(string), alice.ID); err != nil {
		t.Fatal(err)
	}
	refused := `{"device_code":"` + grant["device_code"].(string) + `"}`
	if status, _ := f.call("POST", "/api/v1/sign-in/token", refused); status != http.StatusForbidden {
		t.Fatalf("a refused grant: %d, want 403", status)
	}
	if status, _ := f.call("POST", "/api/v1/sign-in/token", refused); status != http.StatusNotFound {
		t.Fatalf("a refused grant again: %d, want 404", status)
	}
	if status, _ := f.call("POST", "/api/v1/sign-in/token", `{"device_code":"made-up"}`); status != http.StatusNotFound {
		t.Fatalf("an unknown device code: %d, want 404", status)
	}
}
