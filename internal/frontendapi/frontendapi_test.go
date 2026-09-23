package frontendapi_test

import (
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/frontendapi"
	"github.com/csnewman/hangar/internal/workers"
)

func newServer(t *testing.T) *httptest.Server {
	t.Helper()
	d := dbtest.Open(t)
	h, err := frontendapi.New(environments.NewManager(d), workers.NewManager(d),
		slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(h)
	t.Cleanup(srv.Close)
	return srv
}

func do(t *testing.T, srv *httptest.Server, method, path, body string) (int, map[string]any) {
	t.Helper()
	req, _ := http.NewRequest(method, srv.URL+path, strings.NewReader(body))
	if body != "" {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(resp.Body)
	var out map[string]any
	if len(b) > 0 && b[0] == '{' {
		if err := json.Unmarshal(b, &out); err != nil {
			t.Fatalf("%s %s: body is not JSON: %s", method, path, b)
		}
	}
	return resp.StatusCode, out
}

// The spec is enforced before a handler runs, and every refusal comes back
// in the spec's Error shape.
func TestRequestsAreCheckedAgainstTheSpec(t *testing.T) {
	srv := newServer(t)
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
			status, body := do(t, srv, tc.method, tc.path, tc.body)
			if status != tc.status {
				t.Fatalf("status %d, want %d (%v)", status, tc.status, body)
			}
			if msg, _ := body["error"].(string); msg == "" {
				t.Fatalf("no error message in %v", body)
			}
		})
	}
}

func TestEnvironmentRoundTrip(t *testing.T) {
	srv := newServer(t)

	status, env := do(t, srv, "POST", "/api/frontend/environments",
		`{"name":"a","image":"img","cpus":2,"memory_mib":2048}`)
	if status != 201 {
		t.Fatalf("create: %d %v", status, env)
	}
	if env["phase"] != "pending" || env["desired"] != "running" || env["cpus"] != 2.0 {
		t.Fatalf("created %v", env)
	}
	if _, placed := env["worker_id"]; placed {
		t.Fatalf("an unplaced environment reports a worker: %v", env)
	}
	id := env["id"].(string)

	if status, body := do(t, srv, "POST", "/api/frontend/environments",
		`{"name":"a","image":"img","cpus":2,"memory_mib":2048}`); status != 409 {
		t.Fatalf("duplicate name: %d %v", status, body)
	}

	status, env = do(t, srv, "POST", "/api/frontend/environments/"+id+"/stop", "")
	if status != 200 || env["desired"] != "stopped" || env["phase"] != "stopped" {
		t.Fatalf("stop: %d %v", status, env)
	}

	// Nothing holds it, so deleting removes it outright.
	if status, body := do(t, srv, "DELETE", "/api/frontend/environments/"+id, ""); status != 204 {
		t.Fatalf("delete: %d %v", status, body)
	}
	if status, _ := do(t, srv, "GET", "/api/frontend/environments/"+id, ""); status != 404 {
		t.Fatalf("deleted environment still found: %d", status)
	}
}
