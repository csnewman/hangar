package worker_test

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/server"
	"github.com/csnewman/hangar/internal/worker"
)

func TestCode(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	srv, err := server.New(server.Config{DB: dbtest.Open(t), BootstrapToken: token, Log: quiet()})
	if err != nil {
		t.Fatal(err)
	}
	go srv.Run(ctx)
	hs := httptest.NewServer(srv.Handler())
	defer hs.Close()
	signIn(t, srv, hs.URL)

	w, err := worker.New(writeConfig(t, hs.URL), worker.NewSimulated(50*time.Millisecond), quiet())
	if err != nil {
		t.Fatal(err)
	}
	werr := make(chan error, 1)
	go func() { werr <- w.Run(ctx) }()
	defer func() {
		cancel()
		<-werr
	}()

	var tmpl struct{ ID string }
	call(t, hs.URL, http.MethodPost, "/api/frontend/templates", `{"name":"t","visibility":"private","spec":`+
		`{"image":"img","cpus":1,"memory_mib":512,"display":"none","gpu":"none","repos":[],"placement":{},"editor_path":"/workspace/app"}}`, &tmpl)
	var env api.Environment
	call(t, hs.URL, http.MethodPost, "/api/frontend/environments", `{"template_id":"`+tmpl.ID+`","name":"code"}`, &env)
	waitFor(t, hs.URL, env.ID, func(e *api.Environment) bool { return e != nil && e.Phase == api.PhaseRunning })

	u, _ := url.Parse(hs.URL)
	var c *websocket.Conn
	for range 50 {
		c, _, err = websocket.Dial(ctx, strings.Replace(hs.URL, "http", "ws", 1)+"/api/frontend/environments/"+env.ID+"/code",
			&websocket.DialOptions{HTTPHeader: http.Header{"Cookie": {cookieHeader(browser.Jar.Cookies(u))}}})
		if err == nil {
			break
		}
		time.Sleep(100 * time.Millisecond)
	}
	if err != nil {
		t.Fatal(err)
	}
	defer c.CloseNow()

	// Two requests in one go: each message must come back as its own frame.
	for i, method := range []string{"hello", "fs.list"} {
		req, _ := json.Marshal(map[string]any{"id": i + 1, "method": method, "params": map[string]any{"path": "."}})
		if err := c.Write(ctx, websocket.MessageBinary, append([]byte{'r'}, req...)); err != nil {
			t.Fatal(err)
		}
	}
	rctx, rcancel := context.WithTimeout(ctx, 10*time.Second)
	defer rcancel()
	for i := 1; i <= 2; i++ {
		_, msg, err := c.Read(rctx)
		if err != nil {
			t.Fatal(err)
		}
		if msg[0] != 'r' {
			t.Fatalf("channel %q", msg[0])
		}
		var reply struct {
			ID     int             `json:"id"`
			Result json.RawMessage `json:"result"`
		}
		if err := json.Unmarshal(msg[1:], &reply); err != nil || reply.ID != i {
			t.Fatalf("reply %s: %v", msg, err)
		}
		if i == 1 && !strings.Contains(string(reply.Result), `"root":"/workspace/app"`) {
			t.Errorf("hello: %s, want the template's editor path as the root", reply.Result)
		}
	}
}
