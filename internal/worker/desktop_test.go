package worker_test

import (
	"context"
	"encoding/binary"
	"io"
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

func TestDesktop(t *testing.T) {
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

	create := func(name, display string) api.Environment {
		var tmpl struct{ ID string }
		call(t, hs.URL, http.MethodPost, "/api/frontend/templates", `{"name":"`+name+`","visibility":"private","spec":`+
			`{"image":"img","cpus":1,"memory_mib":512,"display":"`+display+`","gpu":"none","repos":[],"placement":{}}}`, &tmpl)
		var env api.Environment
		call(t, hs.URL, http.MethodPost, "/api/frontend/environments", `{"template_id":"`+tmpl.ID+`","name":"`+name+`"}`, &env)
		waitFor(t, hs.URL, env.ID, func(e *api.Environment) bool { return e != nil && e.Phase == api.PhaseRunning })
		return env
	}
	u, _ := url.Parse(hs.URL)
	dial := func(id string) (*websocket.Conn, *http.Response, error) {
		return websocket.Dial(ctx, strings.Replace(hs.URL, "http", "ws", 1)+"/api/frontend/environments/"+id+"/desktop",
			&websocket.DialOptions{
				HTTPHeader:   http.Header{"Cookie": {cookieHeader(browser.Jar.Cookies(u))}},
				Subprotocols: []string{"binary"},
			})
	}

	env := create("desk", "desktop")
	var c *websocket.Conn
	// The worker's tunnel may still be coming up.
	for range 50 {
		if c, _, err = dial(env.ID); err == nil {
			break
		}
		time.Sleep(100 * time.Millisecond)
	}
	if err != nil {
		t.Fatal(err)
	}
	defer c.CloseNow()
	c.SetReadLimit(-1)
	rfb := websocket.NetConn(ctx, c, websocket.MessageBinary)
	rfb.SetDeadline(time.Now().Add(15 * time.Second))

	version := make([]byte, 12)
	if _, err := io.ReadFull(rfb, version); err != nil || string(version) != "RFB 003.008\n" {
		t.Fatalf("server version %q: %v", version, err)
	}
	rfb.Write(version)
	sec := make([]byte, 2)
	io.ReadFull(rfb, sec)
	if sec[0] != 1 || sec[1] != 1 {
		t.Fatalf("security types %v, want only None", sec)
	}
	rfb.Write([]byte{1})
	result := make([]byte, 4)
	io.ReadFull(rfb, result)
	rfb.Write([]byte{1}) // shared
	init := make([]byte, 24)
	if _, err := io.ReadFull(rfb, init); err != nil {
		t.Fatal(err)
	}
	width, height := binary.BigEndian.Uint16(init), binary.BigEndian.Uint16(init[2:])
	name := make([]byte, binary.BigEndian.Uint32(init[20:]))
	io.ReadFull(rfb, name)
	if string(name) != worker.SimulatedDesktopName {
		t.Errorf("desktop name %q", name)
	}

	req := []byte{3, 0, 0, 0, 0, 0}
	req = binary.BigEndian.AppendUint16(req, width)
	req = binary.BigEndian.AppendUint16(req, height)
	rfb.Write(req)
	hdr := make([]byte, 16)
	if _, err := io.ReadFull(rfb, hdr); err != nil {
		t.Fatal(err)
	}
	if hdr[0] != 0 || binary.BigEndian.Uint16(hdr[2:]) != 1 {
		t.Fatalf("update header %v", hdr)
	}
	if n, err := io.CopyN(io.Discard, rfb, int64(width)*int64(height)*4); err != nil {
		t.Fatalf("frame: %d bytes: %v", n, err)
	}

	call(t, hs.URL, http.MethodPut, "/api/frontend/environments/"+env.ID+"/desktop/size", `{"width":1601,"height":900}`, nil)

	headless := create("headless", "none")
	if _, resp, err := dial(headless.ID); err == nil || resp == nil || resp.StatusCode != http.StatusConflict {
		t.Errorf("a headless environment's desktop: %v %v", resp, err)
	}
	put, _ := http.NewRequest(http.MethodPut, hs.URL+"/api/frontend/environments/"+headless.ID+"/desktop/size",
		strings.NewReader(`{"width":800,"height":600}`))
	put.Header.Set("Content-Type", "application/json")
	if resp, err := browser.Do(put); err != nil || resp.StatusCode != http.StatusConflict {
		t.Errorf("resizing a headless environment's desktop: %v %v", resp, err)
	}
}
