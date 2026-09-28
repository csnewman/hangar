package logs_test

import (
	"bytes"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/csnewman/hangar/internal/logs"
)

func TestReadFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "console.log")
	if r, err := logs.ReadFile(path, logs.Request{}); err != nil || !r.Missing {
		t.Fatalf("a log not yet written: %+v, %v", r, err)
	}
	os.WriteFile(path, []byte("one\ntwo\nthree\n"), 0o644)

	// The end, and on from where a reader got to.
	r, err := logs.ReadFile(path, logs.Request{Offset: -1, Max: 6})
	if err != nil || string(r.Data) != "three\n" || r.Start != 8 || r.Size != 14 {
		t.Fatalf("the end: %+v, %v", r, err)
	}
	f, _ := os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0)
	f.WriteString("four\n")
	f.Close()
	r, _ = logs.ReadFile(path, logs.Request{Offset: r.Start + int64(len(r.Data))})
	if string(r.Data) != "four\n" {
		t.Errorf("what followed: %q", r.Data)
	}
	// A reader past the end -- the machine started again with a new log --
	// is given its end.
	r, _ = logs.ReadFile(path, logs.Request{Offset: 1000, Max: 5})
	if string(r.Data) != "four\n" {
		t.Errorf("past the end: %+v", r)
	}
}

// The worker's log is kept whole and per environment, as the handler it
// was written to would have written it.
func TestRings(t *testing.T) {
	var out bytes.Buffer
	rings := logs.NewRings()
	log := slog.New(logs.Tee(slog.NewTextHandler(&out, nil), rings))

	log.Info("worker ready", "name", "w1")
	env := log.With("environment", "e1")
	env.Info("booting")
	log.Warn("opening a stream", "environment", "e2", "err", "gone")
	log.Debug("not kept: below Info, and the handler keeps none")

	whole := string(rings.Read(logs.Request{Offset: 0}).Data)
	if whole != out.String() {
		t.Errorf("the whole log differs from what was written:\n%s\nvs\n%s", whole, out.String())
	}
	e1 := string(rings.Read(logs.Request{Environment: "e1"}).Data)
	if !strings.Contains(e1, "booting") || strings.Contains(e1, "worker ready") || strings.Contains(e1, "opening") {
		t.Errorf("e1's lines: %q", e1)
	}
	if e2 := string(rings.Read(logs.Request{Environment: "e2"}).Data); !strings.Contains(e2, "opening a stream") {
		t.Errorf("e2's lines: %q", e2)
	}
	if !rings.Read(logs.Request{Environment: "e3"}).Missing {
		t.Error("an environment never logged has lines")
	}

	// The oldest go once a ring is full, whole lines at a time.
	for range logs.EnvironmentSize / 10 {
		env.Info("filling")
	}
	r := rings.Read(logs.Request{Environment: "e1", Offset: 0})
	if r.Start == 0 || !bytes.HasPrefix(r.Data, []byte("time=")) || r.Size-r.Start != int64(len(r.Data)) {
		t.Errorf("a full ring: start %d of %d, begins %q", r.Start, r.Size, r.Data[:min(20, len(r.Data))])
	}
}
