package clientapi_test

import (
	"bufio"
	"encoding/json"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/coder/websocket"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/clientapi"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/desktop"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/tunnel"
	"github.com/csnewman/hangar/internal/users"
)

// vnc stands in for a guest's desktop: a VNC connection is greeted as an
// RFB server greets, then echoes; a resize is answered, refusing 1x1.
type vnc struct{ resized chan [2]int }

func (v vnc) Open(_ string, h tunnel.Header) (net.Conn, error) {
	server, client := net.Pipe()
	go func() {
		defer server.Close()
		br := bufio.NewReader(server)
		line, err := br.ReadBytes('\n')
		if err != nil || h.Kind != tunnel.KindDesktop {
			return
		}
		var req desktop.Request
		json.Unmarshal(line, &req)
		switch req.Op {
		case desktop.OpVNC:
			server.Write([]byte("RFB 003.008\n"))
			io.Copy(server, br)
		case desktop.OpResize:
			var reply desktop.Reply
			if req.Width < 2 {
				reply.Err = "too small"
			} else {
				v.resized <- [2]int{req.Width, req.Height}
			}
			b, _ := json.Marshal(reply)
			server.Write(append(b, '\n'))
		}
	}()
	return client, nil
}

// A viewer reaches a running environment's desktop over a WebSocket,
// RFB both ways, and sizes it; a headless one has none.
func TestDesktop(t *testing.T) {
	d := dbtest.Open(t)
	um := users.NewManager(d)
	em := environments.NewManager(d)
	v := vnc{resized: make(chan [2]int, 1)}
	srv := httptest.NewServer(clientapi.New(clientapi.Config{
		Environments: em,
		Users:        um,
		Profiles:     profile.NewStore(d, nil),
		Tunnels:      v,
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
	newEnv := func(name string, display api.Display) api.Environment {
		tmpl, err := templates.NewManager(d).Create(ctx, p, templates.Input{
			Name: name, Spec: api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024, Display: display}},
		})
		if err != nil {
			t.Fatal(err)
		}
		env, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tmpl.ID, Name: name})
		if err != nil {
			t.Fatal(err)
		}
		return env
	}
	env, headless := newEnv("desk", api.DisplayDesktop), newEnv("cli", api.DisplayNone)
	if status, _ := f.call("POST", "/api/v1/environments/"+env.ID+"/desktop/size", `{"width":1280,"height":800}`, bearer); status != 409 {
		t.Fatalf("sizing a desktop not running: %d", status)
	}
	err = d.Transact(ctx, func(tx db.Tx) error {
		var worker string
		if err := tx.QueryRow(ctx, `INSERT INTO workers (name, credential_hash) VALUES ('w', '\x00') RETURNING id`).
			Scan(&worker); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `UPDATE environments SET phase = 'running', worker_id = $1`, worker)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}

	if status, _ := f.call("POST", "/api/v1/environments/"+env.ID+"/desktop/size", `{"width":1280,"height":800}`, bearer); status != 204 {
		t.Fatalf("sizing the desktop: %d", status)
	}
	if got := <-v.resized; got != [2]int{1280, 800} {
		t.Fatalf("sized to %v", got)
	}
	if status, _ := f.call("POST", "/api/v1/environments/"+env.ID+"/desktop/size", `{"width":1,"height":1}`, bearer); status != 409 {
		t.Fatalf("a size refused: %d", status)
	}
	if status, _ := f.call("POST", "/api/v1/environments/"+headless.ID+"/desktop/size", `{"width":1280,"height":800}`, bearer); status != 409 {
		t.Fatalf("sizing a headless environment: %d", status)
	}

	url := "ws" + strings.TrimPrefix(srv.URL, "http") + "/api/v1/environments/" + env.ID + "/desktop"
	if _, resp, err := websocket.Dial(ctx, url, nil); err == nil || resp.StatusCode != 401 {
		t.Fatalf("a socket with nobody signed in: %v", err)
	}
	auth := &websocket.DialOptions{HTTPHeader: http.Header{"Authorization": {"Bearer " + token}}}
	headlessURL := "ws" + strings.TrimPrefix(srv.URL, "http") + "/api/v1/environments/" + headless.ID + "/desktop"
	if _, resp, err := websocket.Dial(ctx, headlessURL, auth); err == nil || resp.StatusCode != 409 {
		t.Fatalf("a headless environment's desktop: %v", err)
	}
	c, _, err := websocket.Dial(ctx, url, auth)
	if err != nil {
		t.Fatal(err)
	}
	defer c.CloseNow()
	_, hello, err := c.Read(ctx)
	if err != nil || string(hello) != "RFB 003.008\n" {
		t.Fatalf("the greeting: %q %v", hello, err)
	}
	if err := c.Write(ctx, websocket.MessageBinary, []byte("RFB 003.008\n")); err != nil {
		t.Fatal(err)
	}
	if _, back, err := c.Read(ctx); err != nil || string(back) != "RFB 003.008\n" {
		t.Fatalf("the echo: %q %v", back, err)
	}
}
