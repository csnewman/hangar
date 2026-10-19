package logs_test

import (
	"bytes"
	"io"
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

// An environment's logs combine as the worker sees them written, each line
// marked with its log: what the files held before it was followed is left
// out, a line waits for its end, and a log that starts over, or is put in
// another file's place, is read again from its start.
func TestCombined(t *testing.T) {
	dir := t.TempDir()
	rings := logs.NewRings()
	log := slog.New(logs.Tee(slog.NewTextHandler(io.Discard, nil), rings)).With("environment", "e1")
	file := func(name string) string { return filepath.Join(dir, name+".log") }
	write := func(name, text string, flag int) {
		f, err := os.OpenFile(file(name), os.O_CREATE|os.O_WRONLY|flag, 0o644)
		if err != nil {
			t.Fatal(err)
		}
		f.WriteString(text)
		f.Close()
	}
	c := logs.NewCombiner(rings, func(env, name string) (string, bool) { return file(name), env == "e1" })
	text := func() string { return string(c.Read(logs.Request{Environment: "e1", Offset: 0}).Data) }

	if !c.Read(logs.Request{Environment: "e1"}).Missing {
		t.Error("an environment never followed has a combined log")
	}
	write(logs.Console, "an earlier boot\n", os.O_APPEND)
	log.Info("starting")
	c.Poll()
	write(logs.Console, "Linux version\r\nlogin: ", os.O_APPEND)
	write(logs.Monitor, "monitor up\n", os.O_APPEND)
	c.Poll()
	log.Info("started")
	write(logs.Console, "dev\n", os.O_APPEND)
	c.Poll()

	got := text()
	for _, want := range []string{"worker  │ time=", "msg=starting", "console │ Linux version\n",
		"monitor │ monitor up\n", "msg=started", "console │ login: dev\n"} {
		if !strings.Contains(got, want) {
			t.Errorf("no %q in:\n%s", want, got)
		}
	}
	if strings.Contains(got, "an earlier boot") {
		t.Errorf("what the console held before it was followed is there:\n%s", got)
	}
	if strings.Index(got, "Linux version") > strings.Index(got, "msg=started") {
		t.Errorf("out of the order written:\n%s", got)
	}

	// The machine boots again, its console starting over.
	write(logs.Console, "booting again\n", os.O_TRUNC)
	c.Poll()
	if got := text(); !strings.HasSuffix(got, "console │ booting again\n") {
		t.Errorf("after the console started over:\n%s", got)
	}

	// And again, its monitor's log a new file already longer than the last.
	os.Remove(file(logs.Monitor))
	write(logs.Monitor, "monitor up again, with more to say\n", os.O_APPEND)
	c.Poll()
	if got := text(); !strings.HasSuffix(got, "monitor │ monitor up again, with more to say\n") {
		t.Errorf("after the monitor's log was made anew:\n%s", got)
	}
}
