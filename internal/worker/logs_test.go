package worker_test

import (
	"context"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/blob"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/logs"
	"github.com/csnewman/hangar/internal/server"
	"github.com/csnewman/hangar/internal/worker"
)

// An environment's logs, and its worker's, are read from the worker through
// the control plane: the worker's own log whole for an administrator, and
// its lines about one environment for whoever may use it.
func TestLogs(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	srv, err := server.New(server.Config{DB: dbtest.Open(t), Blobs: blob.NewMemory(), ProfileBlobs: blob.NewMemory(), Files: t.TempDir(), BootstrapToken: token, Log: quiet()})
	if err != nil {
		t.Fatal(err)
	}
	go srv.Run(ctx)
	hs := httptest.NewServer(srv.Handler())
	defer hs.Close()
	signIn(t, srv, hs.URL)

	rings := logs.NewRings()
	log := slog.New(logs.Tee(slog.NewTextHandler(io.Discard, nil), rings))
	w, err := worker.New(writeConfig(t, hs.URL), worker.NewSimulated(50*time.Millisecond), log)
	if err != nil {
		t.Fatal(err)
	}
	w.UseLogs(rings)
	werr := make(chan error, 1)
	go func() { werr <- w.Run(ctx) }()
	defer func() {
		cancel()
		<-werr
	}()

	var tmpl struct{ ID string }
	call(t, hs.URL, http.MethodPost, "/api/frontend/templates", `{"name":"t","visibility":"private","spec":`+
		`{"image":"img","cpus":1,"memory_mib":512,"display":"none","gpu":"none","repos":[],"placement":{}}}`, &tmpl)
	var env api.Environment
	call(t, hs.URL, http.MethodPost, "/api/frontend/environments", `{"template_id":"`+tmpl.ID+`","name":"logged"}`, &env)
	waitFor(t, hs.URL, env.ID, func(e *api.Environment) bool { return e != nil && e.Phase == api.PhaseRunning })
	call(t, hs.URL, http.MethodGet, "/api/frontend/environments/"+env.ID, "", &env)

	log.Info("booting", "environment", env.ID)
	log.Info("something else", "environment", "another")

	type part struct {
		Start, Next, Size int64
		Text              string
		Missing           bool
	}
	var got part
	call(t, hs.URL, http.MethodGet, "/api/frontend/environments/"+env.ID+"/logs/worker", "", &got)
	if !strings.Contains(got.Text, "booting") || strings.Contains(got.Text, "something else") || got.Next != got.Start+int64(len(got.Text)) {
		t.Errorf("the environment's worker log: %+v", got)
	}
	// Read on from there: nothing new yet, then what follows.
	var more part
	call(t, hs.URL, http.MethodGet, "/api/frontend/environments/"+env.ID+"/logs/worker?offset="+itoa(got.Next), "", &more)
	if more.Text != "" {
		t.Errorf("nothing new, but read %q", more.Text)
	}
	log.Info("booted", "environment", env.ID)
	call(t, hs.URL, http.MethodGet, "/api/frontend/environments/"+env.ID+"/logs/worker?offset="+itoa(got.Next), "", &more)
	if !strings.Contains(more.Text, "booted") || strings.Contains(more.Text, "booting") {
		t.Errorf("what followed: %q", more.Text)
	}

	// The simulated runtime has no machine, so no console.
	var console part
	call(t, hs.URL, http.MethodGet, "/api/frontend/environments/"+env.ID+"/logs/console", "", &console)
	if !console.Missing {
		t.Errorf("a console with no machine: %+v", console)
	}

	var whole part
	call(t, hs.URL, http.MethodGet, "/api/frontend/workers/"+env.WorkerID+"/log", "", &whole)
	if !strings.Contains(whole.Text, "worker ready") || !strings.Contains(whole.Text, "something else") {
		t.Errorf("the worker's whole log: %q", whole.Text)
	}
}

func itoa(n int64) string { return strconv.FormatInt(n, 10) }
