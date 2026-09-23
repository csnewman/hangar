package frontendapi_test

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/cookiejar"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/frontendapi"
	"github.com/csnewman/hangar/internal/users"
	"github.com/csnewman/hangar/internal/workers"
)

const password = "correct-horse"

type fixture struct {
	srv   *httptest.Server
	users *users.Manager
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	d := dbtest.Open(t)
	um := users.NewManager(d)
	h, err := frontendapi.New(frontendapi.Config{
		Environments: environments.NewManager(d),
		Workers:      workers.NewManager(d),
		Users:        um,
		Log:          slog.New(slog.NewTextHandler(io.Discard, nil)),
	})
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(h)
	t.Cleanup(srv.Close)
	return &fixture{srv: srv, users: um}
}

// client is one browser: it keeps its own cookies.
type client struct {
	t    *testing.T
	base string
	http *http.Client
}

func (f *fixture) anonymous(t *testing.T) *client {
	jar, _ := cookiejar.New(nil)
	return &client{t: t, base: f.srv.URL, http: &http.Client{Jar: jar}}
}

// user creates a user and returns a client signed in as them.
func (f *fixture) user(t *testing.T, name string, admin bool) (*client, string) {
	t.Helper()
	u, err := f.users.Create(context.Background(), users.NewUser{Username: name, Password: password, Admin: admin})
	if err != nil {
		t.Fatal(err)
	}
	c := f.anonymous(t)
	if status, body := c.do("POST", "/api/frontend/auth/login",
		`{"username":"`+name+`","password":"`+password+`"}`); status != 200 {
		t.Fatalf("login as %s: %d %v", name, status, body)
	}
	return c, u.ID
}

func (c *client) do(method, path, body string, headers ...string) (int, map[string]any) {
	c.t.Helper()
	req, _ := http.NewRequest(method, c.base+path, strings.NewReader(body))
	if body != "" {
		req.Header.Set("Content-Type", "application/json")
	}
	for i := 0; i+1 < len(headers); i += 2 {
		req.Header.Set(headers[i], headers[i+1])
	}
	resp, err := c.http.Do(req)
	if err != nil {
		c.t.Fatal(err)
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(resp.Body)
	var out map[string]any
	if len(b) > 0 && b[0] == '{' {
		if err := json.Unmarshal(b, &out); err != nil {
			c.t.Fatalf("%s %s: body is not JSON: %s", method, path, b)
		}
	}
	if len(b) > 0 && b[0] == '[' {
		var list []map[string]any
		if err := json.Unmarshal(b, &list); err != nil {
			c.t.Fatal(err)
		}
		out = map[string]any{"items": list}
	}
	return resp.StatusCode, out
}

func (c *client) items(path string) []map[string]any {
	c.t.Helper()
	status, body := c.do("GET", path, "")
	if status != 200 {
		c.t.Fatalf("GET %s: %d %v", path, status, body)
	}
	list, _ := body["items"].([]map[string]any)
	return list
}

func (c *client) create(name string) string {
	c.t.Helper()
	status, env := c.do("POST", "/api/frontend/environments",
		`{"name":"`+name+`","image":"img","cpus":1,"memory_mib":1024}`)
	if status != 201 {
		c.t.Fatalf("create %s: %d %v", name, status, env)
	}
	return env["id"].(string)
}

// The spec is enforced before a handler runs, and every refusal comes back
// in the spec's Error shape.
func TestRequestsAreCheckedAgainstTheSpec(t *testing.T) {
	f := newFixture(t)
	c, _ := f.user(t, "alice", false)
	for name, tc := range map[string]struct {
		method, path, body string
		status             int
	}{
		"missing field":     {"POST", "/api/frontend/environments", `{"name":"a","image":"i","cpus":1}`, 400},
		"unknown field":     {"POST", "/api/frontend/environments", `{"name":"a","image":"i","cpus":1,"memory_mib":1024,"gpu":true}`, 400},
		"name pattern":      {"POST", "/api/frontend/environments", `{"name":"Not A Label","image":"i","cpus":1,"memory_mib":1024}`, 400},
		"cpus over maximum": {"POST", "/api/frontend/environments", `{"name":"a","image":"i","cpus":65,"memory_mib":1024}`, 400},
		"wrong type":        {"POST", "/api/frontend/environments", `{"name":"a","image":"i","cpus":"two","memory_mib":1024}`, 400},
		"unknown endpoint":  {"GET", "/api/frontend/nothing", "", 404},
		"unknown id":        {"GET", "/api/frontend/environments/5c7b9f4c-250f-49d2-b276-c9b127230208", "", 404},
		"malformed id":      {"POST", "/api/frontend/environments/nope/stop", "", 404},
	} {
		t.Run(name, func(t *testing.T) {
			c.t = t
			status, body := c.do(tc.method, tc.path, tc.body)
			if status != tc.status {
				t.Fatalf("status %d, want %d (%v)", status, tc.status, body)
			}
			if msg, _ := body["error"].(string); msg == "" {
				t.Fatalf("no error message in %v", body)
			}
		})
	}
}

func TestSignInAndOut(t *testing.T) {
	f := newFixture(t)
	anon := f.anonymous(t)

	if status, _ := anon.do("GET", "/api/frontend/environments", ""); status != 401 {
		t.Fatalf("anonymous list: %d, want 401", status)
	}
	if status, _ := anon.do("GET", "/api/frontend/me", ""); status != 401 {
		t.Fatalf("anonymous me: %d, want 401", status)
	}

	if _, err := f.users.Create(context.Background(), users.NewUser{Username: "Alice", Password: password}); err != nil {
		t.Fatal(err)
	}
	for _, bad := range []string{
		`{"username":"alice","password":"wrong-password"}`,
		`{"username":"nobody","password":"` + password + `"}`,
	} {
		status, body := anon.do("POST", "/api/frontend/auth/login", bad)
		if status != 401 || body["error"] != users.ErrBadCredentials.Error() {
			t.Fatalf("bad login %s: %d %v", bad, status, body)
		}
	}

	// Usernames are case-insensitive.
	status, me := anon.do("POST", "/api/frontend/auth/login", `{"username":"ALICE","password":"`+password+`"}`)
	if status != 200 || me["username"] != "Alice" || me["admin"] != false {
		t.Fatalf("login: %d %v", status, me)
	}
	if status, _ := anon.do("GET", "/api/frontend/me", ""); status != 200 {
		t.Fatalf("me after login: %d", status)
	}
	if status, _ := anon.do("POST", "/api/frontend/auth/logout", ""); status != 204 {
		t.Fatalf("logout: %d", status)
	}
	if status, _ := anon.do("GET", "/api/frontend/me", ""); status != 401 {
		t.Fatalf("me after logout: %d, want 401", status)
	}
}

// A user reaches only their own environments, and another user's look
// exactly like ones that do not exist.
func TestUsersSeeOnlyTheirOwnEnvironments(t *testing.T) {
	f := newFixture(t)
	alice, _ := f.user(t, "alice", false)
	bob, _ := f.user(t, "bob", false)
	admin, _ := f.user(t, "root", true)

	a := alice.create("dev")
	// Names are per owner.
	b := bob.create("dev")

	if list := alice.items("/api/frontend/environments"); len(list) != 1 || list[0]["id"] != a {
		t.Fatalf("alice sees %v", list)
	}
	for _, op := range []struct{ method, path string }{
		{"GET", "/api/frontend/environments/" + b},
		{"POST", "/api/frontend/environments/" + b + "/stop"},
		{"POST", "/api/frontend/environments/" + b + "/start"},
		{"DELETE", "/api/frontend/environments/" + b},
	} {
		if status, body := alice.do(op.method, op.path, ""); status != 404 {
			t.Fatalf("alice %s %s: %d %v, want 404", op.method, op.path, status, body)
		}
	}

	list := admin.items("/api/frontend/environments")
	if len(list) != 2 {
		t.Fatalf("admin sees %d environments, want 2", len(list))
	}
	owners := map[string]bool{}
	for _, e := range list {
		owners[e["owner"].(string)] = true
	}
	if !owners["alice"] || !owners["bob"] {
		t.Fatalf("owners %v", owners)
	}
	if status, env := admin.do("POST", "/api/frontend/environments/"+b+"/stop", ""); status != 200 || env["desired"] != "stopped" {
		t.Fatalf("admin stopping bob's environment: %d %v", status, env)
	}
}

func TestAdminOnlyOperations(t *testing.T) {
	f := newFixture(t)
	alice, _ := f.user(t, "alice", false)
	admin, _ := f.user(t, "root", true)

	for _, path := range []string{"/api/frontend/workers", "/api/frontend/users"} {
		if status, _ := alice.do("GET", path, ""); status != 403 {
			t.Fatalf("user GET %s: %d, want 403", path, status)
		}
		if status, _ := admin.do("GET", path, ""); status != 200 {
			t.Fatalf("admin GET %s: %d, want 200", path, status)
		}
	}
	if status, _ := alice.do("POST", "/api/frontend/users", `{"username":"eve","password":"password1","admin":true}`); status != 403 {
		t.Fatalf("user creating an admin: %d, want 403", status)
	}
}

func TestCrossSiteWritesAreRefused(t *testing.T) {
	f := newFixture(t)
	alice, _ := f.user(t, "alice", false)
	body := `{"name":"x","image":"i","cpus":1,"memory_mib":1024}`
	if status, _ := alice.do("POST", "/api/frontend/environments", body, "Sec-Fetch-Site", "cross-site"); status != 403 {
		t.Fatalf("cross-site POST: %d, want 403", status)
	}
	if status, _ := alice.do("POST", "/api/frontend/environments", body, "Origin", "https://evil.example"); status != 403 {
		t.Fatalf("foreign-origin POST: %d, want 403", status)
	}
	if status, _ := alice.do("POST", "/api/frontend/environments", body, "Sec-Fetch-Site", "same-origin"); status != 201 {
		t.Fatalf("same-origin POST: %d, want 201", status)
	}
}

func TestUserManagement(t *testing.T) {
	f := newFixture(t)
	admin, adminID := f.user(t, "root", true)
	alice, aliceID := f.user(t, "alice", false)

	// The last administrator cannot be demoted, disabled or removed.
	for _, change := range []string{`{"admin":false}`, `{"disabled":true}`} {
		if status, body := admin.do("PATCH", "/api/frontend/users/"+adminID, change); status != 409 {
			t.Fatalf("%s on the last admin: %d %v, want 409", change, status, body)
		}
	}
	if status, _ := admin.do("DELETE", "/api/frontend/users/"+adminID, ""); status != 409 {
		t.Fatalf("deleting the last admin: %d, want 409", status)
	}

	// Disabling a user ends their session at once.
	if status, body := admin.do("PATCH", "/api/frontend/users/"+aliceID, `{"disabled":true}`); status != 200 || body["disabled"] != true {
		t.Fatalf("disable: %d %v", status, body)
	}
	if status, _ := alice.do("GET", "/api/frontend/me", ""); status != 401 {
		t.Fatalf("disabled user's session still works: %d", status)
	}
	if status, _ := f.anonymous(t).do("POST", "/api/frontend/auth/login",
		`{"username":"alice","password":"`+password+`"}`); status != 401 {
		t.Fatalf("disabled user signed in: %d", status)
	}

	// A user who owns environments cannot be deleted.
	if status, body := admin.do("PATCH", "/api/frontend/users/"+aliceID, `{"disabled":false}`); status != 200 {
		t.Fatalf("enable: %d %v", status, body)
	}
	alice, _ = f.anonymous(t), ""
	if status, _ := alice.do("POST", "/api/frontend/auth/login", `{"username":"alice","password":"`+password+`"}`); status != 200 {
		t.Fatalf("re-enabled user cannot sign in: %d", status)
	}
	alice.create("keep")
	if status, _ := admin.do("DELETE", "/api/frontend/users/"+aliceID, ""); status != 409 {
		t.Fatalf("deleting a user with environments: %d, want 409", status)
	}
}

// Changing your password keeps the session you changed it from and ends
// every other.
func TestChangePassword(t *testing.T) {
	f := newFixture(t)
	here, _ := f.user(t, "alice", false)
	elsewhere := f.anonymous(t)
	if status, _ := elsewhere.do("POST", "/api/frontend/auth/login", `{"username":"alice","password":"`+password+`"}`); status != 200 {
		t.Fatal("second login failed")
	}

	if status, _ := here.do("POST", "/api/frontend/me/password",
		`{"current_password":"wrong-one","new_password":"new-password"}`); status != 400 {
		t.Fatalf("wrong current password: %d, want 400", status)
	}
	if status, body := here.do("POST", "/api/frontend/me/password",
		`{"current_password":"`+password+`","new_password":"new-password"}`); status != 204 {
		t.Fatalf("change: %d %v", status, body)
	}
	if status, _ := here.do("GET", "/api/frontend/me", ""); status != 200 {
		t.Fatalf("the changing session ended: %d", status)
	}
	if status, _ := elsewhere.do("GET", "/api/frontend/me", ""); status != 401 {
		t.Fatalf("another session survived a password change: %d", status)
	}
}
