package clientapi_test

import (
	"bufio"
	"encoding/binary"
	"encoding/json"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"os/exec"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/clientapi"
	"github.com/csnewman/hangar/internal/code"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/logs"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/terminal"
	"github.com/csnewman/hangar/internal/tunnel"
	"github.com/csnewman/hangar/internal/users"
)

// guest stands in for a worker: each terminal stream is a pipe to one
// terminal manager, whose shell echoes what it is sent; its logs are the
// console's one line.
type guest struct{ m *terminal.Manager }

func (g guest) Open(_ string, h tunnel.Header) (net.Conn, error) {
	server, client := net.Pipe()
	switch h.Kind {
	case tunnel.KindCode:
		go codeService(server)
	case tunnel.KindLogs:
		go logService(server)
	default:
		go g.m.Serve(server)
	}
	return client, nil
}

// logService answers a logs request: the console has one line, from the
// offset asked for; there is no GPU log.
func logService(c net.Conn) {
	defer c.Close()
	line, err := bufio.NewReader(c).ReadBytes('\n')
	if err != nil {
		return
	}
	var req logs.Request
	json.Unmarshal(line, &req)
	text := "booted " + req.Environment + "\n"
	reply := logs.Reply{Missing: req.Log == logs.GPU}
	if !reply.Missing {
		start := max(req.Offset, 0)
		reply = logs.Reply{Start: start, Size: int64(len(text)), Data: []byte(text[min(start, int64(len(text))):])}
	}
	b, _ := json.Marshal(reply)
	c.Write(append(b, '\n'))
}

// codeService stands in for the Code service: after the request line, it
// answers each JSON frame with its root and the method asked for.
func codeService(c net.Conn) {
	defer c.Close()
	br := bufio.NewReader(c)
	line, err := br.ReadBytes('\n')
	if err != nil {
		return
	}
	var req code.Request
	json.Unmarshal(line, &req)
	for {
		var hdr [4]byte
		if _, err := io.ReadFull(br, hdr[:]); err != nil {
			return
		}
		msg := make([]byte, binary.BigEndian.Uint32(hdr[:]))
		if _, err := io.ReadFull(br, msg); err != nil {
			return
		}
		var in struct {
			ID     int    `json:"id"`
			Method string `json:"method"`
		}
		json.Unmarshal(msg[1:], &in)
		out, _ := json.Marshal(map[string]any{"id": in.ID, "result": req.Root + " " + in.Method})
		frame := append([]byte{'r'}, out...)
		binary.BigEndian.PutUint32(hdr[:], uint32(len(frame)))
		c.Write(append(hdr[:], frame...))
	}
}

// A client reads an environment's logs, lists its terminals, attaches to a
// new one over a WebSocket, types into it and reads it back, and ends it.
func TestTerminals(t *testing.T) {
	d := dbtest.Open(t)
	um := users.NewManager(d)
	em := environments.NewManager(d)
	m := terminal.NewManager(func(terminal.Request) (*exec.Cmd, error) {
		return exec.Command("/bin/sh", "-c", `echo hello; while read l; do echo "got:$l"; done`), nil
	}, slog.New(slog.NewTextHandler(io.Discard, nil)))
	srv := httptest.NewServer(clientapi.New(clientapi.Config{
		Environments: em,
		Users:        um,
		Profiles:     profile.NewStore(d, nil),
		Tunnels:      guest{m},
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

	if status, _ := f.call("GET", "/api/v1/environments/"+env.ID+"/terminals", "", bearer); status != 409 {
		t.Fatalf("terminals of an environment not running: %d", status)
	}
	if status, _ := f.call("GET", "/api/v1/environments/"+env.ID+"/logs/console", "", bearer); status != 409 {
		t.Fatalf("logs of an environment on no worker: %d", status)
	}
	// Running, on a worker.
	err = d.Transact(ctx, func(tx db.Tx) error {
		var worker string
		if err := tx.QueryRow(ctx, `INSERT INTO workers (name, credential_hash) VALUES ('w', '\x00') RETURNING id`).
			Scan(&worker); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `UPDATE environments SET phase = 'running', worker_id = $2 WHERE id = $1`, env.ID, worker)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}

	// Its logs, from its worker: the end, then on from an offset.
	status, body := f.call("GET", "/api/v1/environments/"+env.ID+"/logs/console", "", bearer)
	part, _ := body.(map[string]any)
	if status != 200 || part["text"] != "booted "+env.ID+"\n" || part["start"] != 0.0 || part["next"] != part["size"] {
		t.Fatalf("the console: %d %v", status, body)
	}
	status, body = f.call("GET", "/api/v1/environments/"+env.ID+"/logs/console?offset=7", "", bearer)
	if part, _ := body.(map[string]any); status != 200 || part["text"] != env.ID+"\n" || part["start"] != 7.0 {
		t.Fatalf("on from 7: %d %v", status, body)
	}
	status, body = f.call("GET", "/api/v1/environments/"+env.ID+"/logs/gpu", "", bearer)
	if part, _ := body.(map[string]any); status != 200 || part["missing"] != true {
		t.Fatalf("no GPU log: %d %v", status, body)
	}
	if status, _ := f.call("GET", "/api/v1/environments/"+env.ID+"/logs/console", ""); status != 401 {
		t.Fatalf("logs with nobody signed in: %d", status)
	}

	url := "ws" + strings.TrimPrefix(srv.URL, "http") + "/api/v1/environments/" + env.ID + "/terminal?cols=80&rows=24"
	if _, resp, err := websocket.Dial(ctx, url, nil); err == nil || resp.StatusCode != 401 {
		t.Fatalf("a socket with nobody signed in: %v", err)
	}
	c, _, err := websocket.Dial(ctx, url, &websocket.DialOptions{HTTPHeader: http.Header{"Authorization": {"Bearer " + token}}})
	if err != nil {
		t.Fatal(err)
	}
	defer c.CloseNow()
	_, first, err := c.Read(ctx)
	if err != nil {
		t.Fatal(err)
	}
	var reply terminal.Reply
	if err := json.Unmarshal(first, &reply); err != nil || reply.Session == nil {
		t.Fatalf("the attach's reply: %s %v", first, err)
	}
	if err := c.Write(ctx, websocket.MessageBinary, append([]byte{terminal.FrameInput}, "one\n"...)); err != nil {
		t.Fatal(err)
	}
	var out strings.Builder
	deadline := time.Now().Add(5 * time.Second)
	for !strings.Contains(out.String(), "got:one") {
		if time.Now().After(deadline) {
			t.Fatalf("output: %q", out.String())
		}
		_, msg, err := c.Read(ctx)
		if err != nil {
			t.Fatal(err)
		}
		if len(msg) > 0 && msg[0] == terminal.FrameOutput {
			out.Write(msg[1:])
		}
	}

	status, body = f.call("GET", "/api/v1/environments/"+env.ID+"/terminals", "", bearer)
	if list, _ := body.([]any); status != 200 || len(list) != 1 || list[0].(map[string]any)["id"] != reply.Session.ID {
		t.Fatalf("terminals: %d %v", status, body)
	}
	if status, _ := f.call("DELETE", "/api/v1/environments/"+env.ID+"/terminals/"+reply.Session.ID, "", bearer); status != 204 {
		t.Fatalf("ending it: %d", status)
	}
	if status, _ := f.call("DELETE", "/api/v1/environments/"+env.ID+"/terminals/nope", "", bearer); status != 404 {
		t.Fatalf("ending no session: %d", status)
	}

	// The Code service, in a folder asked for, one frame a message.
	codeURL := "ws" + strings.TrimPrefix(srv.URL, "http") + "/api/v1/environments/" + env.ID + "/code?root=/home/dev/app"
	cc, _, err := websocket.Dial(ctx, codeURL, &websocket.DialOptions{HTTPHeader: http.Header{"Authorization": {"Bearer " + token}}})
	if err != nil {
		t.Fatal(err)
	}
	defer cc.CloseNow()
	if err := cc.Write(ctx, websocket.MessageBinary, []byte(`r{"id":1,"method":"workspace.get"}`)); err != nil {
		t.Fatal(err)
	}
	_, msg, err := cc.Read(ctx)
	if err != nil || string(msg) != `r{"id":1,"result":"/home/dev/app workspace.get"}` {
		t.Fatalf("the service's answer: %q %v", msg, err)
	}
}
