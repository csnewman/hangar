package clientapi_test

import (
	"bufio"
	"encoding/json"
	"io"
	"log/slog"
	"net"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/clientapi"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/procs"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/tunnel"
	"github.com/csnewman/hangar/internal/users"
)

// agent stands in for a worker's processes stream: init, protected, and
// one program, which a signal stops.
type agent struct{}

func (agent) Open(_ string, h tunnel.Header) (net.Conn, error) {
	server, client := net.Pipe()
	go func() {
		defer server.Close()
		line, err := bufio.NewReader(server).ReadBytes('\n')
		if err != nil || h.Kind != tunnel.KindProcesses {
			return
		}
		var req procs.Request
		json.Unmarshal(line, &req)
		var reply procs.Reply
		switch {
		case req.Op == "list":
			reply = procs.Reply{CPUs: 2, MemoryBytes: 2 << 30, Processes: []procs.Process{
				{PID: 1, Name: "init", Command: "/sbin/init", State: "S", Protected: true, Started: time.Unix(0, 0)},
				{PID: 42, PPID: 1, User: "dev", Name: "node", Command: "node server.js", State: "R", CPUPercent: 12.5,
					RSSBytes: 64 << 20, Threads: 7, Started: time.Unix(100, 0)},
			}}
		case req.PID == 1:
			reply = procs.Reply{Error: "init cannot be stopped"}
		}
		b, _ := json.Marshal(reply)
		server.Write(append(b, '\n'))
	}()
	return client, nil
}

// A client lists a running environment's processes, stops one, is
// refused stopping init, reads what it uses, and reads its activity,
// the stop among it.
func TestProcessesStatsAndActivity(t *testing.T) {
	d := dbtest.Open(t)
	um := users.NewManager(d)
	em := environments.NewManager(d)
	srv := httptest.NewServer(clientapi.New(clientapi.Config{
		Environments: em,
		Users:        um,
		Profiles:     profile.NewStore(d, nil),
		Tunnels:      agent{},
		Audit:        audit.NewLog(d),
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
	base := "/api/v1/environments/" + env.ID

	if status, _ := f.call("GET", base+"/processes", "", bearer); status != 409 {
		t.Fatalf("processes of an environment not running: %d", status)
	}
	err = d.Transact(ctx, func(tx db.Tx) error {
		var worker string
		if err := tx.QueryRow(ctx, `INSERT INTO workers (name, credential_hash) VALUES ('w', '\x00') RETURNING id`).
			Scan(&worker); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `UPDATE environments SET phase = 'running', worker_id = $2,
			stats = '{"cpu_percent": 37.5, "memory_used_mib": 700, "memory_total_mib": 2048, "net_rx_bps": 1000}'
			WHERE id = $1`, env.ID, worker)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}

	status, body := f.call("GET", base+"/processes", "", bearer)
	list, _ := body.(map[string]any)
	ps, _ := list["processes"].([]any)
	if status != 200 || list["cpus"] != 2.0 || len(ps) != 2 {
		t.Fatalf("the processes: %d %v", status, body)
	}
	node := ps[1].(map[string]any)
	if node["pid"] != 42.0 || node["cpu_percent"] != 12.5 || node["command"] != "node server.js" || node["protected"] != nil {
		t.Fatalf("node: %v", node)
	}
	if ps[0].(map[string]any)["protected"] != true {
		t.Fatalf("init is protected: %v", ps[0])
	}
	if status, _ := f.call("POST", base+"/processes/42/signal", `{"signal":"TERM"}`, bearer); status != 204 {
		t.Fatalf("stopping node: %d", status)
	}
	if status, body := f.call("POST", base+"/processes/1/signal", `{"signal":"KILL"}`, bearer); status != 409 {
		t.Fatalf("stopping init: %d %v", status, body)
	}
	if status, _ := f.call("POST", base+"/processes/42/signal", `{"signal":"HUP"}`, bearer); status != 400 {
		t.Fatalf("a signal not offered: %d", status)
	}

	status, body = f.call("GET", base, "", bearer)
	stats, _ := body.(map[string]any)["stats"].(map[string]any)
	if status != 200 || stats["cpu_percent"] != 37.5 || stats["memory_used_mib"] != 700.0 || stats["net_rx_bps"] != 1000.0 {
		t.Fatalf("what it uses: %d %v", status, body)
	}

	status, body = f.call("GET", base+"/activity?limit=10", "", bearer)
	events, _ := body.([]any)
	var stopped bool
	for _, e := range events {
		e := e.(map[string]any)
		details, _ := e["details"].(map[string]any)
		if e["action"] == "environment.signal_process" && details["pid"] == 42.0 {
			stopped = true
		}
	}
	if status != 200 || !stopped {
		t.Fatalf("its activity: %d %v", status, body)
	}
	if status, _ := f.call("GET", "/api/v1/environments/nope/activity", "", bearer); status != 404 {
		t.Fatalf("activity of no environment: %d", status)
	}
}
