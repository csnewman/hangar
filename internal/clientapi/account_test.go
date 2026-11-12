package clientapi_test

import (
	"bytes"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/csnewman/hangar/internal/clientapi"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/users"
)

// A client lists the user's access tokens, the one it signs in with
// marked, revokes another, makes and removes an SSH key, and removes a
// sign-in key; nobody else's can be removed.
func TestAccount(t *testing.T) {
	d := dbtest.Open(t)
	um := users.NewManager(d)
	sealer, err := profile.NewSealer(bytes.Repeat([]byte{7}, 32))
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(clientapi.New(clientapi.Config{
		Environments: environments.NewManager(d),
		Users:        um,
		Profiles:     profile.NewStore(d, sealer),
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
	_, other, err := um.CreateToken(ctx, u.ID, "ci", 0)
	if err != nil {
		t.Fatal(err)
	}
	auth := "Authorization: Bearer " + token

	status, body := f.call("GET", "/api/v1/me/tokens", "", auth)
	if status != http.StatusOK {
		t.Fatalf("listing tokens: %d %v", status, body)
	}
	list := body.([]any)
	if len(list) != 2 {
		t.Fatalf("tokens: %v", list)
	}
	for _, x := range list {
		tok := x.(map[string]any)
		if want := tok["name"] == "desktop"; tok["current"] != want {
			t.Errorf("token %v: current %v, want %v", tok["name"], tok["current"], want)
		}
	}
	if status, _ := f.call("DELETE", "/api/v1/me/tokens/"+other.ID, "", auth); status != http.StatusNoContent {
		t.Fatalf("revoking a token: %d", status)
	}
	if status, _ := f.call("DELETE", "/api/v1/me/tokens/"+other.ID, "", auth); status != http.StatusNotFound {
		t.Fatalf("revoking it again: %d, want 404", status)
	}
	if _, body := f.call("GET", "/api/v1/me/tokens", "", auth); len(body.([]any)) != 1 {
		t.Fatalf("tokens after revoking: %v", body)
	}

	status, body = f.call("POST", "/api/v1/me/ssh-keys", `{"name":"github"}`, auth)
	if status != http.StatusCreated {
		t.Fatalf("making an SSH key: %d %v", status, body)
	}
	key := body.(map[string]any)
	if key["name"] != "github" || key["public_key"] == "" || key["fingerprint"] == "" {
		t.Fatalf("SSH key: %v", key)
	}
	if _, body := f.call("GET", "/api/v1/me/ssh-keys", "", auth); len(body.([]any)) != 1 {
		t.Fatalf("SSH keys: %v", body)
	}
	if status, _ := f.call("DELETE", "/api/v1/me/ssh-keys/"+key["id"].(string), "", auth); status != http.StatusNoContent {
		t.Fatalf("removing the SSH key: %d", status)
	}
	if _, body := f.call("GET", "/api/v1/me/ssh-keys", "", auth); len(body.([]any)) != 0 {
		t.Fatalf("SSH keys after removing: %v", body)
	}

	pub := "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl laptop"
	status, body = f.call("POST", "/api/v1/me/login-keys", `{"public_key":"`+pub+`"}`, auth)
	if status != http.StatusCreated {
		t.Fatalf("adding a sign-in key: %d %v", status, body)
	}
	id := body.(map[string]any)["id"].(string)

	bob, err := um.Create(ctx, users.NewUser{Username: "bob", Password: "correct-horse"})
	if err != nil {
		t.Fatal(err)
	}
	bobs, _, err := um.CreateToken(ctx, bob.ID, "laptop", 0)
	if err != nil {
		t.Fatal(err)
	}
	if status, _ := f.call("DELETE", "/api/v1/me/login-keys/"+id, "", "Authorization: Bearer "+bobs); status != http.StatusNotFound {
		t.Fatalf("someone else removing the key: %d, want 404", status)
	}
	if status, _ := f.call("DELETE", "/api/v1/me/login-keys/"+id, "", auth); status != http.StatusNoContent {
		t.Fatalf("removing the sign-in key: %d", status)
	}
	if _, body := f.call("GET", "/api/v1/me/login-keys", "", auth); len(body.([]any)) != 0 {
		t.Fatalf("sign-in keys after removing: %v", body)
	}
}
