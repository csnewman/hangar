package erofs_test

import (
	"archive/tar"
	"bytes"
	"fmt"
	"io"
	"io/fs"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"testing/fstest"
	"time"

	"github.com/csnewman/hangar/internal/erofs"
)

// want is what a path in a built image should be.
type want struct {
	path     string
	typ      erofs.Type
	perm     uint32
	uid, gid uint32
	mtime    time.Time
	data     []byte
	target   string
	major    uint32
	minor    uint32
	xattrs   map[string][]byte
	nlink    uint32
}

var epoch = time.Date(2026, 9, 1, 12, 30, 45, 123456789, time.UTC)

// pattern is n bytes that differ block to block, so data in the wrong
// place shows.
func pattern(n int, seed byte) []byte {
	b := make([]byte, n)
	for i := range b {
		b[i] = byte(i*7+i/4096) ^ seed
	}
	return b
}

// tree is a tree that exercises the format: files of every size around a
// block, names that sort before ".", a directory of several blocks,
// attributes, links and devices, and a file before its directory.
func tree() []want {
	ws := []want{
		{path: ".", typ: erofs.TypeDir, perm: 0o755},
		{path: "etc", typ: erofs.TypeDir, perm: 0o755, mtime: epoch},
		{path: "etc/empty", typ: erofs.TypeRegular, perm: 0o644, mtime: epoch, data: []byte{}},
		{path: "etc/one", typ: erofs.TypeRegular, perm: 0o600, uid: 1000, gid: 1000, mtime: epoch, data: []byte("x")},
		{path: "etc/block-1", typ: erofs.TypeRegular, perm: 0o644, mtime: epoch, data: pattern(4095, 1)},
		{path: "etc/block", typ: erofs.TypeRegular, perm: 0o644, mtime: epoch, data: pattern(4096, 2)},
		{path: "etc/block+1", typ: erofs.TypeRegular, perm: 0o644, mtime: epoch, data: pattern(4097, 3)},
		{path: "etc/blocks", typ: erofs.TypeRegular, perm: 0o755, mtime: epoch, data: pattern(3*4096+123, 4)},
		{path: "etc/big", typ: erofs.TypeRegular, perm: 0o644, mtime: epoch, data: pattern(3<<20+17, 5)},
		// A tail that would fit alone, but not beside these attributes.
		{path: "etc/crowded", typ: erofs.TypeRegular, perm: 0o644, mtime: epoch, data: pattern(4096+3000, 6),
			xattrs: map[string][]byte{"user.pad": bytes.Repeat([]byte("p"), 1500)}},
		// Attributes larger than a block.
		{path: "etc/huge-xattrs", typ: erofs.TypeRegular, perm: 0o644, mtime: epoch, data: []byte("hi"),
			xattrs: map[string][]byte{"user.a": bytes.Repeat([]byte("a"), 3000), "user.b": bytes.Repeat([]byte("b"), 3000)}},
		{path: "usr/bin/ping", typ: erofs.TypeRegular, perm: 0o755, mtime: epoch, data: []byte("#!/bin/sh\n"),
			xattrs: map[string][]byte{"security.capability": {1, 0, 0, 2, 0, 0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0}}},
		{path: "usr", typ: erofs.TypeDir, perm: 0o755, uid: 0, gid: 0, mtime: epoch},
		{path: "usr/bin/sudo", typ: erofs.TypeRegular, perm: 0o4755, mtime: epoch, data: []byte("sudo")},
		{path: "tmp", typ: erofs.TypeDir, perm: 0o1777, mtime: epoch},
		{path: "home/dev", typ: erofs.TypeDir, perm: 0o750, uid: 100000, gid: 100000, mtime: epoch,
			xattrs: map[string][]byte{"user.note": []byte("v"), "trusted.t": []byte("t")}},
		{path: "bin", typ: erofs.TypeSymlink, perm: 0o777, mtime: epoch, target: "usr/bin"},
		{path: "etc/abs", typ: erofs.TypeSymlink, perm: 0o777, mtime: epoch, target: "/usr/bin/sudo"},
		// A target too long to keep with its inode.
		{path: "etc/long-link", typ: erofs.TypeSymlink, perm: 0o777, mtime: epoch, target: strings.Repeat("./", 2022) + "one"},
		{path: "dev/null", typ: erofs.TypeCharDevice, perm: 0o666, mtime: epoch, major: 1, minor: 3},
		{path: "dev/sda1", typ: erofs.TypeBlockDevice, perm: 0o660, mtime: epoch, major: 8, minor: 300},
		{path: "dev/fifo", typ: erofs.TypeFIFO, perm: 0o644, mtime: epoch},
		{path: "sort/-dash", typ: erofs.TypeRegular, perm: 0o644, mtime: epoch, data: []byte("1")},
		{path: "sort/!bang", typ: erofs.TypeRegular, perm: 0o644, mtime: epoch, data: []byte("2")},
		{path: "sort/" + strings.Repeat("n", 255), typ: erofs.TypeRegular, perm: 0o644, mtime: epoch, data: []byte("3")},
	}
	for i := range 700 {
		ws = append(ws, want{path: fmt.Sprintf("many/file-%04d-%s", i, strings.Repeat("z", i%40)),
			typ: erofs.TypeRegular, perm: 0o644, mtime: epoch, data: []byte(fmt.Sprint(i))})
	}
	return ws
}

func tarOf(t *testing.T, ws []want, links map[string]string) []byte {
	t.Helper()
	var buf bytes.Buffer
	tw := tar.NewWriter(&buf)
	for _, w := range ws {
		hdr := &tar.Header{Name: w.path, Mode: int64(w.perm), Uid: int(w.uid), Gid: int(w.gid),
			ModTime: w.mtime, Format: tar.FormatPAX}
		switch w.typ {
		case erofs.TypeRegular:
			hdr.Typeflag, hdr.Size = tar.TypeReg, int64(len(w.data))
		case erofs.TypeDir:
			hdr.Typeflag = tar.TypeDir
		case erofs.TypeSymlink:
			hdr.Typeflag, hdr.Linkname = tar.TypeSymlink, w.target
		case erofs.TypeCharDevice:
			hdr.Typeflag, hdr.Devmajor, hdr.Devminor = tar.TypeChar, int64(w.major), int64(w.minor)
		case erofs.TypeBlockDevice:
			hdr.Typeflag, hdr.Devmajor, hdr.Devminor = tar.TypeBlock, int64(w.major), int64(w.minor)
		case erofs.TypeFIFO:
			hdr.Typeflag = tar.TypeFifo
		}
		if len(w.xattrs) > 0 {
			hdr.PAXRecords = map[string]string{}
			for k, v := range w.xattrs {
				hdr.PAXRecords["SCHILY.xattr."+k] = string(v)
			}
		}
		if err := tw.WriteHeader(hdr); err != nil {
			t.Fatal(err)
		}
		if w.typ == erofs.TypeRegular {
			tw.Write(w.data)
		}
	}
	for _, name := range slices.Sorted(maps.Keys(links)) {
		if err := tw.WriteHeader(&tar.Header{Name: name, Typeflag: tar.TypeLink, Linkname: links[name]}); err != nil {
			t.Fatal(err)
		}
	}
	if err := tw.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

// build writes an image with add, and opens it.
func build(t *testing.T, add func(*erofs.Writer) error) (*erofs.Image, string) {
	t.Helper()
	p := filepath.Join(t.TempDir(), "image.erofs")
	f, err := os.Create(p)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	w := erofs.NewWriter(f)
	if err := add(w); err != nil {
		t.Fatal(err)
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}
	st, _ := f.Stat()
	if st.Size() != w.Size() || w.Size()%erofs.BlockSize != 0 {
		t.Fatalf("the file is %d bytes, the image says %d", st.Size(), w.Size())
	}
	g, err := os.Open(p)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { g.Close() })
	im, err := erofs.Open(g)
	if err != nil {
		t.Fatal(err)
	}
	return im, p
}

func check(t *testing.T, im *erofs.Image, ws []want) {
	t.Helper()
	for _, w := range ws {
		in, err := im.Lookup(w.path, false)
		if err != nil {
			t.Errorf("%s: %v", w.path, err)
			continue
		}
		if in.Type != w.typ || in.Perm != w.perm || in.UID != w.uid || in.GID != w.gid {
			t.Errorf("%s: %v %o %d:%d, want %v %o %d:%d", w.path, in.Type, in.Perm, in.UID, in.GID,
				w.typ, w.perm, w.uid, w.gid)
		}
		if !w.mtime.IsZero() && !in.ModTime.Equal(w.mtime) {
			t.Errorf("%s: modified %v, want %v", w.path, in.ModTime, w.mtime)
		}
		if w.nlink != 0 && in.Nlink != w.nlink {
			t.Errorf("%s: %d links, want %d", w.path, in.Nlink, w.nlink)
		}
		switch w.typ {
		case erofs.TypeRegular:
			got, err := io.ReadAll(in.Data())
			if err != nil || !bytes.Equal(got, w.data) {
				t.Errorf("%s: %d bytes (%v), want %d", w.path, len(got), err, len(w.data))
			}
			// A read from the middle, across the inline tail's boundary.
			if n := len(w.data); n > 10 {
				mid := make([]byte, n/2)
				if _, err := in.Data().ReadAt(mid, int64(n-len(mid))); err != nil && err != io.EOF ||
					!bytes.Equal(mid, w.data[n-len(mid):]) {
					t.Errorf("%s: reading its second half: %v", w.path, err)
				}
			}
		case erofs.TypeSymlink:
			if got, err := in.Readlink(); err != nil || got != w.target {
				t.Errorf("%s: points to %q (%v), want %q", w.path, got, err, w.target)
			}
		case erofs.TypeCharDevice, erofs.TypeBlockDevice:
			if in.Major != w.major || in.Minor != w.minor {
				t.Errorf("%s: device %d:%d, want %d:%d", w.path, in.Major, in.Minor, w.major, w.minor)
			}
		}
		got, err := in.Xattrs()
		if err != nil {
			t.Errorf("%s: %v", w.path, err)
		}
		if len(got) != len(w.xattrs) || !maps.EqualFunc(got, w.xattrs, bytes.Equal) {
			t.Errorf("%s: attributes %v, want %v", w.path, slices.Sorted(maps.Keys(got)), slices.Sorted(maps.Keys(w.xattrs)))
		}
	}
}

func TestTar(t *testing.T) {
	ws := tree()
	links := map[string]string{"usr/bin/sudoedit": "usr/bin/sudo", "etc/again": "etc/block+1"}
	im, _ := build(t, func(w *erofs.Writer) error {
		return w.AddTar(tar.NewReader(bytes.NewReader(tarOf(t, ws, links))))
	})
	check(t, im, ws)

	// A directory made for what it holds before its own entry.
	check(t, im, []want{
		{path: "usr/bin", typ: erofs.TypeDir, perm: 0o755, nlink: 2},
		{path: "dev", typ: erofs.TypeDir, perm: 0o755},
		{path: "home", typ: erofs.TypeDir, perm: 0o755},
	})

	a, _ := im.Lookup("usr/bin/sudoedit", false)
	b, _ := im.Lookup("usr/bin/sudo", false)
	if a == nil || b == nil || a.Nid != b.Nid || a.Nlink != 2 {
		t.Errorf("a hard link is not the same inode: %v %v", a, b)
	}
	if root, _ := im.Root(); root.Nlink != 2+7 {
		t.Errorf("the root has %d links, want one for each of its 7 directories and 2", root.Nlink)
	}

	// Symlinks resolve inside the image.
	if in, err := im.Lookup("bin/sudo", true); err != nil || in.Nid != b.Nid {
		t.Errorf("bin/sudo through a relative link: %v %v", in, err)
	}
	if in, err := im.Lookup("etc/abs", true); err != nil || in.Nid != b.Nid {
		t.Errorf("etc/abs through an absolute link: %v %v", in, err)
	}

	root, _ := im.Root()
	ents, err := root.ReadDir()
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, e := range ents {
		names = append(names, e.Name)
	}
	if want := []string{"bin", "dev", "etc", "home", "many", "sort", "tmp", "usr"}; !slices.Equal(names, want) {
		t.Errorf("the root holds %v, want %v", names, want)
	}

	var expected []string
	for _, w := range ws {
		if w.path != "." && w.typ != erofs.TypeSymlink {
			expected = append(expected, w.path)
		}
	}
	if err := fstest.TestFS(im.FS(), expected...); err != nil {
		t.Error(err)
	}
	data, err := fs.ReadFile(im.FS(), "bin/sudo")
	if err != nil || string(data) != "sudo" {
		t.Errorf("reading through the FS: %q %v", data, err)
	}
}

func TestDir(t *testing.T) {
	dir := t.TempDir()
	os.MkdirAll(filepath.Join(dir, "a/b"), 0o755)
	os.WriteFile(filepath.Join(dir, "a/b/file"), pattern(10000, 9), 0o640)
	os.WriteFile(filepath.Join(dir, "small"), []byte("small"), 0o600)
	os.Link(filepath.Join(dir, "small"), filepath.Join(dir, "a/hard"))
	os.Symlink("a/b/file", filepath.Join(dir, "link"))
	os.Chmod(filepath.Join(dir, "a"), 0o711)
	when := time.Date(2025, 1, 2, 3, 4, 5, 600000000, time.UTC)
	os.Chtimes(filepath.Join(dir, "small"), when, when)

	im, _ := build(t, func(w *erofs.Writer) error { return w.AddDir(dir) })
	uid, gid := uint32(os.Getuid()), uint32(os.Getgid())
	// A symlink's own mode is always 0777 on Linux, but not on macOS.
	st, _ := os.Lstat(filepath.Join(dir, "link"))
	check(t, im, []want{
		{path: "a", typ: erofs.TypeDir, perm: 0o711, uid: uid, gid: gid},
		{path: "a/b/file", typ: erofs.TypeRegular, perm: 0o640, uid: uid, gid: gid, data: pattern(10000, 9)},
		{path: "small", typ: erofs.TypeRegular, perm: 0o600, uid: uid, gid: gid, data: []byte("small"), mtime: when, nlink: 2},
		{path: "link", typ: erofs.TypeSymlink, perm: uint32(st.Mode().Perm()), uid: uid, gid: gid, target: "a/b/file"},
	})
	a, _ := im.Lookup("a/hard", false)
	b, _ := im.Lookup("small", false)
	if a == nil || b == nil || a.Nid != b.Nid {
		t.Errorf("a hard link on disk is not one inode")
	}
}

func TestRefused(t *testing.T) {
	f, err := os.Create(filepath.Join(t.TempDir(), "x"))
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	w := erofs.NewWriter(f)
	file := erofs.Entry{Path: "a/f", Type: erofs.TypeRegular, Perm: 0o644, Size: 1}
	if err := w.Add(file, strings.NewReader("x")); err != nil {
		t.Fatal(err)
	}
	for name, err := range map[string]error{
		"the same file twice":     w.Add(file, strings.NewReader("x")),
		"a file under a file":     w.Add(erofs.Entry{Path: "a/f/g", Type: erofs.TypeRegular}, strings.NewReader("")),
		"a directory over a file": w.Add(erofs.Entry{Path: "a/f", Type: erofs.TypeDir}, nil),
		"a link to nothing":       w.Link("a/l", "a/nothing"),
		"a link to a directory":   w.Link("a/l", "a"),
		"a short file":            w.Add(erofs.Entry{Path: "short", Type: erofs.TypeRegular, Size: 10}, strings.NewReader("x")),
		"a name too long":         w.Add(erofs.Entry{Path: strings.Repeat("n", 256), Type: erofs.TypeDir}, nil),
		"an attribute too large":  w.Add(erofs.Entry{Path: "x", Type: erofs.TypeDir, Xattrs: map[string][]byte{"user.x": make([]byte, 70000)}}, nil),
		"a file as the root":      w.Add(erofs.Entry{Path: "/", Type: erofs.TypeRegular}, strings.NewReader("")),
		"a directory added twice ": func() error {
			w.Add(erofs.Entry{Path: "d", Type: erofs.TypeDir}, nil)
			return w.Add(erofs.Entry{Path: "d", Type: erofs.TypeDir}, nil)
		}(),
	} {
		if err == nil {
			t.Errorf("%s: accepted", name)
		}
	}
	// A path that climbs out stays in the root.
	if err := w.Add(erofs.Entry{Path: "../../escape", Type: erofs.TypeRegular, Size: 0}, strings.NewReader("")); err != nil {
		t.Fatal(err)
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}
	im, err := erofs.Open(f)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := im.Lookup("escape", false); err != nil {
		t.Errorf("../../escape is not /escape: %v", err)
	}
}
