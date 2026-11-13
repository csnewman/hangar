package erofs_test

import (
	"bytes"
	"encoding/json"
	"flag"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/erofs"
)

var update = flag.Bool("update", false, "write the golden image from this writer")

// The fixture the Rust writer (crates/erofs) is held to as well: the same
// tree must make the same image, byte for byte.
const (
	fixture = "../../crates/erofs/tests/data/tree.json"
	golden  = "../../crates/erofs/tests/data/golden.erofs"
)

type fixtureEntry struct {
	Path   string            `json:"path"`
	Type   string            `json:"type"`
	Perm   uint32            `json:"perm"`
	UID    uint32            `json:"uid"`
	GID    uint32            `json:"gid"`
	MTime  int64             `json:"mtime"`
	Nsec   int64             `json:"nsec"`
	Text   *string           `json:"text"`
	Fill   int64             `json:"fill"`
	Target string            `json:"target"`
	Major  uint32            `json:"major"`
	Minor  uint32            `json:"minor"`
	Xattrs map[string]string `json:"xattrs"`
}

// fill is n bytes that repeat only every 251.
func fill(n int64) []byte {
	b := make([]byte, n)
	for i := range b {
		b[i] = byte((i*7 + 3) % 251)
	}
	return b
}

func TestGoldenImage(t *testing.T) {
	raw, err := os.ReadFile(fixture)
	if err != nil {
		t.Fatal(err)
	}
	var entries []fixtureEntry
	if err := json.Unmarshal(raw, &entries); err != nil {
		t.Fatal(err)
	}
	f, err := os.Create(t.TempDir() + "/image")
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	w := erofs.NewWriter(f)
	types := map[string]erofs.Type{
		"file": erofs.TypeRegular, "dir": erofs.TypeDir, "symlink": erofs.TypeSymlink,
		"char": erofs.TypeCharDevice, "block": erofs.TypeBlockDevice, "fifo": erofs.TypeFIFO,
		"socket": erofs.TypeSocket,
	}
	for _, fe := range entries {
		if fe.Type == "link" {
			if err := w.Link(fe.Path, fe.Target); err != nil {
				t.Fatal(err)
			}
			continue
		}
		e := erofs.Entry{
			Path: fe.Path, Type: types[fe.Type], Perm: fe.Perm, UID: fe.UID, GID: fe.GID,
			ModTime: time.Unix(fe.MTime, fe.Nsec), Target: fe.Target, Major: fe.Major, Minor: fe.Minor,
		}
		if len(fe.Xattrs) > 0 {
			e.Xattrs = map[string][]byte{}
			for k, v := range fe.Xattrs {
				e.Xattrs[k] = []byte(v)
			}
		}
		var data []byte
		switch {
		case fe.Text != nil:
			data = []byte(*fe.Text)
		case fe.Fill > 0:
			data = fill(fe.Fill)
		}
		e.Size = int64(len(data))
		if err := w.Add(e, bytes.NewReader(data)); err != nil {
			t.Fatal(err)
		}
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}
	got, err := os.ReadFile(f.Name())
	if err != nil {
		t.Fatal(err)
	}
	got = got[:w.Size()]
	if *update {
		if err := os.WriteFile(golden, got, 0o644); err != nil {
			t.Fatal(err)
		}
		return
	}
	want, err := os.ReadFile(golden)
	if err != nil {
		t.Fatalf("%v (go test -run TestGoldenImage -update writes it)", err)
	}
	if !bytes.Equal(got, want) {
		at := 0
		for at < min(len(got), len(want)) && got[at] == want[at] {
			at++
		}
		t.Fatalf("the image differs from %s: %d bytes against %d, first at %d",
			strings.TrimPrefix(golden, "../../"), len(got), len(want), at)
	}
}
