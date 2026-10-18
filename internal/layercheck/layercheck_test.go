package layercheck_test

import (
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"testing/fstest"

	"github.com/csnewman/hangar/internal/erofs"
	"github.com/csnewman/hangar/internal/layercheck"
)

type file struct {
	typ    erofs.Type
	perm   uint32
	uid    uint32
	data   string
	target string
}

// image writes an EROFS image of files and opens it.
func image(t *testing.T, files map[string]file) *erofs.Image {
	t.Helper()
	f, err := os.Create(filepath.Join(t.TempDir(), "image.erofs"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { f.Close() })
	w := erofs.NewWriter(f)
	for p, fl := range files {
		e := erofs.Entry{Path: p, Type: fl.typ, Perm: fl.perm, UID: fl.uid, Target: fl.target}
		if fl.typ == erofs.TypeRegular {
			e.Size = int64(len(fl.data))
		}
		if err := w.Add(e, strings.NewReader(fl.data)); err != nil {
			t.Fatal(err)
		}
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}
	im, err := erofs.Open(f)
	if err != nil {
		t.Fatal(err)
	}
	return im
}

// layer is an upper directory: a whiteout is a character device, and
// opaque names the directories overlayfs takes as opaque.
type layer struct {
	fstest.MapFS
	opaque []string
}

func (l layer) Opaque(dir string) (bool, error) { return slices.Contains(l.opaque, dir), nil }

func reg(data string) file       { return file{typ: erofs.TypeRegular, perm: 0o644, data: data} }
func link(target string) file    { return file{typ: erofs.TypeSymlink, perm: 0o777, target: target} }
func upperFile() *fstest.MapFile { return &fstest.MapFile{Data: []byte("mine"), Mode: 0o644} }

var whiteout = &fstest.MapFile{Mode: fs.ModeDevice | fs.ModeCharDevice}

func TestCompare(t *testing.T) {
	older := image(t, map[string]file{
		"etc/changed":         reg("old"),
		"etc/same":            reg("same"),
		"etc/gone":            reg("old"),
		"etc/kept":            reg("kept"),
		"etc/mode":            reg("x"),
		"etc/link":            link("a"),
		"opt/opaque/a":        reg("a"),
		"opt/still/a":         reg("a"),
		"var/lib/dpkg/status": reg("old packages"),
		"tmp/scratch":         reg("old"),
		"usr/lib/thing":       reg("old"),
		"lib":                 link("usr/lib"),
	})
	newer := image(t, map[string]file{
		"etc/changed":         reg("new"),
		"etc/same":            reg("same"),
		"etc/kept":            reg("kept"),
		"etc/mode":            {typ: erofs.TypeRegular, perm: 0o600, data: "x"},
		"etc/link":            link("b"),
		"etc/added":           reg("new"),
		"opt/opaque/a":        reg("a"),
		"opt/opaque/b":        reg("b"),
		"opt/still/a":         reg("a"),
		"var/lib/dpkg/status": reg("new packages"),
		"tmp/scratch":         reg("new"),
		"usr/lib/thing":       reg("new"),
		"lib":                 link("usr/lib"),
	})
	l := layer{MapFS: fstest.MapFS{
		"etc/changed":         upperFile(),
		"etc/same":            upperFile(),
		"etc/gone":            whiteout,
		"etc/kept":            whiteout,
		"etc/mode":            upperFile(),
		"etc/link":            upperFile(),
		"etc/mine":            upperFile(),
		"opt/opaque":          {Mode: fs.ModeDir | 0o755},
		"opt/still":           {Mode: fs.ModeDir | 0o755},
		"var/lib/dpkg/status": upperFile(),
		"tmp/scratch":         upperFile(),
		// Through a symlink in the image, as the guest resolves it.
		"lib/thing": upperFile(),
	}, opaque: []string{"opt/opaque", "opt/still"}}

	got, err := layercheck.Compare(l, older, newer)
	if err != nil {
		t.Fatal(err)
	}
	slices.Sort(got.Conflicts)
	want := []string{"/etc/changed", "/etc/gone", "/etc/link", "/etc/mode", "/lib/thing", "/opt/opaque/", "/var/lib/dpkg/status"}
	if !slices.Equal(got.Conflicts, want) {
		t.Errorf("conflicts %q, want %q", got.Conflicts, want)
	}
	if !got.Packages {
		t.Error("the package database is not reported")
	}

	// A layer that touches nothing the images differ in hides nothing.
	quiet := layer{MapFS: fstest.MapFS{"etc/same": upperFile(), "etc/mine": upperFile()}}
	got, err = layercheck.Compare(quiet, older, newer)
	if err != nil || len(got.Conflicts) != 0 || got.Packages {
		t.Errorf("a quiet layer: %+v %v", got, err)
	}
}
