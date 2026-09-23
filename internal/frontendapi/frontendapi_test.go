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
	"github.com/csnewman/hangar/internal/templates"
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
		Templates:    templates.NewManager(d),
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

const basicSpec = `{"image":"img","cpus":1,"memory_mib":1024,"display":"none","gpu":"none","repos":[],"placement":{}}`

// template creates a template and returns its ID. extra is spliced into the
// body, for a name pattern or visibility.
func (c *client) template(name, extra string) string {
	c.t.Helper()
	body := `{"name":"` + name + `","visibility":"private","spec":` + basicSpec + extra + `}`
	status, t := c.do("POST", "/api/frontend/templates", body)
	if status != 201 {
		c.t.Fatalf("create template %s: %d %v", name, status, t)
	}
	return t["id"].(string)
}

// create makes an environment from a template of the client's own.
func (c *client) create(name string) string {
	c.t.Helper()
	tmpl := c.template("for "+name, "")
	status, env := c.do("POST", "/api/frontend/environments",
		`{"template_id":"`+tmpl+`","name":"`+name+`"}`)
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
		"missing field":     {"POST", "/api/frontend/environments", `{"name":"a"}`, 400},
		"unknown field":     {"POST", "/api/frontend/environments", `{"name":"a","template_id":"x","gpu":true}`, 400},
		"name pattern":      {"POST", "/api/frontend/environments", `{"name":"Not A Label","template_id":"x"}`, 400},
		"unknown template":  {"POST", "/api/frontend/environments", `{"name":"a","template_id":"5c7b9f4c-250f-49d2-b276-c9b127230208"}`, 400},
		"cpus over maximum": {"POST", "/api/frontend/templates", `{"name":"t","visibility":"private","spec":{"image":"i","cpus":65,"memory_mib":1024,"display":"none","gpu":"none","repos":[],"placement":{}}}`, 400},
		"wrong type":        {"POST", "/api/frontend/templates", `{"name":"t","visibility":"private","spec":{"image":"i","cpus":"two","memory_mib":1024,"display":"none","gpu":"none","repos":[],"placement":{}}}`, 400},
		"bad gpu":           {"POST", "/api/frontend/templates", `{"name":"t","visibility":"private","spec":{"image":"i","cpus":1,"memory_mib":1024,"display":"none","gpu":"lots","repos":[],"placement":{}}}`, 400},
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
	body := `{"name":"x","visibility":"private","spec":` + basicSpec + `}`
	if status, _ := alice.do("POST", "/api/frontend/templates", body, "Sec-Fetch-Site", "cross-site"); status != 403 {
		t.Fatalf("cross-site POST: %d, want 403", status)
	}
	if status, _ := alice.do("POST", "/api/frontend/templates", body, "Origin", "https://evil.example"); status != 403 {
		t.Fatalf("foreign-origin POST: %d, want 403", status)
	}
	if status, _ := alice.do("POST", "/api/frontend/templates", body, "Sec-Fetch-Site", "same-origin"); status != 201 {
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

// Private templates are their owner's and collaborators'; shared ones are
// everyone's to use, but still only theirs to change.
func TestTemplateAccess(t *testing.T) {
	f := newFixture(t)
	alice, _ := f.user(t, "alice", false)
	bob, bobID := f.user(t, "bob", false)
	carol, carolID := f.user(t, "carol", false)

	private := alice.template("mine", "")
	shared := alice.template("ours", "")
	status, body := alice.do("PUT", "/api/frontend/templates/"+shared,
		`{"name":"ours","visibility":"shared","spec":`+basicSpec+`}`)
	if status != 200 || body["visibility"] != "shared" {
		t.Fatalf("share: %d %v", status, body)
	}

	names := func(c *client) map[string]bool {
		out := map[string]bool{}
		for _, t := range c.items("/api/frontend/templates") {
			out[t["name"].(string)] = true
		}
		return out
	}
	if got := names(bob); got["mine"] || !got["ours"] {
		t.Fatalf("bob sees %v, want only ours", got)
	}
	if status, _ := bob.do("GET", "/api/frontend/templates/"+private, ""); status != 404 {
		t.Fatalf("bob reading alice's private template: %d, want 404", status)
	}

	// Bob may use the shared template but not change it.
	if status, body := bob.do("POST", "/api/frontend/environments", `{"template_id":"`+shared+`","name":"b1"}`); status != 201 {
		t.Fatalf("bob using a shared template: %d %v", status, body)
	}
	if status, _ := bob.do("PUT", "/api/frontend/templates/"+shared,
		`{"name":"hijacked","visibility":"shared","spec":`+basicSpec+`}`); status != 403 {
		t.Fatalf("bob editing alice's shared template: %d, want 403", status)
	}
	if status, _ := bob.do("POST", "/api/frontend/environments", `{"template_id":"`+private+`","name":"b2"}`); status != 400 {
		t.Fatalf("bob using alice's private template: %d, want 400", status)
	}

	// A collaborator sees and edits a private template, and may bring
	// others in, but may neither share it nor delete it.
	if status, body := alice.do("PUT", "/api/frontend/templates/"+private+"/collaborators",
		`{"user_ids":["`+bobID+`"]}`); status != 200 {
		t.Fatalf("add bob: %d %v", status, body)
	}
	status, body = bob.do("PUT", "/api/frontend/templates/"+private,
		`{"name":"mine","description":"edited by bob","visibility":"private","spec":`+basicSpec+`}`)
	if status != 200 || body["description"] != "edited by bob" || body["can_manage"] != false {
		t.Fatalf("collaborator edit: %d %v", status, body)
	}
	if status, _ := bob.do("PUT", "/api/frontend/templates/"+private,
		`{"name":"mine","visibility":"shared","spec":`+basicSpec+`}`); status != 403 {
		t.Fatalf("collaborator sharing: %d, want 403", status)
	}
	if status, _ := bob.do("DELETE", "/api/frontend/templates/"+private, ""); status != 403 {
		t.Fatalf("collaborator deleting: %d, want 403", status)
	}
	if status, body := bob.do("PUT", "/api/frontend/templates/"+private+"/collaborators",
		`{"user_ids":["`+bobID+`","`+carolID+`"]}`); status != 200 {
		t.Fatalf("collaborator adding carol: %d %v", status, body)
	}
	if got := names(carol); !got["mine"] {
		t.Fatalf("carol, now a collaborator, sees %v", got)
	}

	// Bob taking himself off leaves him unable to see it, and the change
	// stands.
	if status, body := bob.do("PUT", "/api/frontend/templates/"+private+"/collaborators",
		`{"user_ids":["`+carolID+`"]}`); status != 200 {
		t.Fatalf("bob leaving: %d %v", status, body)
	}
	if got := names(bob); got["mine"] {
		t.Fatal("bob still sees the template he left")
	}

	// Deleting a template leaves its environments, with its name.
	_, e := alice.do("POST", "/api/frontend/environments", `{"template_id":"`+private+`","name":"kept"}`)
	if status, _ := alice.do("DELETE", "/api/frontend/templates/"+private, ""); status != 204 {
		t.Fatalf("owner delete: %d", status)
	}
	status, got := alice.do("GET", "/api/frontend/environments/"+e["id"].(string), "")
	if status != 200 || got["template"] != "mine" || got["template_id"] != nil {
		t.Fatalf("environment after its template went: %d %v", status, got)
	}
}

// A template can demand a name, and name a branch after it.
func TestNamePatternAndBranch(t *testing.T) {
	f := newFixture(t)
	alice, _ := f.user(t, "alice", false)
	spec := `{"image":"img","cpus":1,"memory_mib":1024,"display":"desktop","gpu":"none",` +
		`"repos":[{"url":"https://github.com/example/app.git","path":"/workspace/app","branch":"agent/{name}"}],` +
		`"editor_path":"/workspace/app","placement":{}}`
	status, tmpl := alice.do("POST", "/api/frontend/templates",
		`{"name":"tickets","visibility":"private","spec":`+spec+`,"name_pattern":"proj-[0-9]+","name_hint":"a ticket, like proj-123"}`)
	if status != 201 {
		t.Fatalf("create: %d %v", status, tmpl)
	}
	id := tmpl["id"].(string)

	// The pattern must match the whole name.
	for _, bad := range []string{"fix-login", "proj-12x", "xproj-12"} {
		status, body := alice.do("POST", "/api/frontend/environments", `{"template_id":"`+id+`","name":"`+bad+`"}`)
		if status != 400 || !strings.Contains(body["error"].(string), "a ticket, like proj-123") {
			t.Fatalf("name %s: %d %v", bad, status, body)
		}
	}
	status, env := alice.do("POST", "/api/frontend/environments", `{"template_id":"`+id+`","name":"proj-42"}`)
	if status != 201 {
		t.Fatalf("create env: %d %v", status, env)
	}
	repos := env["spec"].(map[string]any)["repos"].([]any)
	if branch := repos[0].(map[string]any)["branch"]; branch != "agent/proj-42" {
		t.Fatalf("branch %v, want agent/proj-42", branch)
	}

	// Changing the template later leaves the environment as it was made.
	if status, body := alice.do("PUT", "/api/frontend/templates/"+id,
		`{"name":"tickets","visibility":"private","spec":`+basicSpec+`}`); status != 200 {
		t.Fatalf("edit: %d %v", status, body)
	}
	_, env = alice.do("GET", "/api/frontend/environments/"+env["id"].(string), "")
	if len(env["spec"].(map[string]any)["repos"].([]any)) != 1 {
		t.Fatalf("editing the template changed an existing environment: %v", env["spec"])
	}

	// Validation of the recipe itself.
	for name, bad := range map[string]string{
		"bad regexp":   `,"name_pattern":"proj-[0-9"`,
		"bad repo url": ``,
	} {
		body := `{"name":"x","visibility":"private","spec":` + basicSpec + bad + `}`
		if name == "bad repo url" {
			body = `{"name":"x","visibility":"private","spec":{"image":"img","cpus":1,"memory_mib":1024,"display":"none","gpu":"none",` +
				`"repos":[{"url":"not a url","path":"/w"}],"placement":{}}}`
		}
		if status, got := alice.do("POST", "/api/frontend/templates", body); status != 400 {
			t.Fatalf("%s: %d %v, want 400", name, status, got)
		}
	}
}
