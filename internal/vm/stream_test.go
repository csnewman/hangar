package vm_test

import (
	"archive/tar"
	"bytes"
	"context"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/csnewman/hangar/internal/vm"
)

type entry struct {
	name, body, link string
	typ              byte
	mode             int64
	uid              int
	xattrs           map[string]string
}

func layer(t *testing.T, es ...entry) []byte {
	t.Helper()
	var buf bytes.Buffer
	w := tar.NewWriter(&buf)
	for _, e := range es {
		hdr := &tar.Header{Name: e.name, Typeflag: e.typ, Mode: e.mode, Uid: e.uid, Linkname: e.link, Format: tar.FormatPAX}
		if hdr.Typeflag == 0 {
			hdr.Typeflag = tar.TypeReg
		}
		if hdr.Mode == 0 {
			hdr.Mode = 0o644
			if hdr.Typeflag == tar.TypeDir {
				hdr.Mode = 0o755
			}
		}
		if hdr.Typeflag == tar.TypeReg {
			hdr.Size = int64(len(e.body))
		}
		for k, v := range e.xattrs {
			if hdr.PAXRecords == nil {
				hdr.PAXRecords = map[string]string{}
			}
			hdr.PAXRecords["SCHILY.xattr."+k] = v
		}
		if err := w.WriteHeader(hdr); err != nil {
			t.Fatal(err)
		}
		if hdr.Typeflag == tar.TypeReg {
			if _, err := io.WriteString(w, e.body); err != nil {
				t.Fatal(err)
			}
		}
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

// An image's layers make one EROFS image of the tree they describe:
// what an upper layer changes, removes or makes opaque is as it says, and
// what it leaves is the lower layer's.
func TestBuildEROFS(t *testing.T) {
	for _, tool := range []string{"mkfs.erofs", "fsck.erofs"} {
		if _, err := exec.LookPath(tool); err != nil {
			t.Skip(tool + " is not installed")
		}
	}
	long := strings.Repeat("d", 120) + "/" + strings.Repeat("f", 120)
	bottom := layer(t,
		entry{name: "./", typ: tar.TypeDir, mode: 0o755},
		entry{name: "etc/", typ: tar.TypeDir},
		entry{name: "usr/bin/tool", body: "tool\n", mode: 0o755},
		entry{name: "etc/a", body: "base\n"},
		entry{name: "etc/kept", body: "kept\n", uid: 1000},
		entry{name: "etc/gone", body: "gone\n"},
		entry{name: "opt/gonedir/", typ: tar.TypeDir},
		entry{name: "opt/gonedir/f", body: "f\n"},
		entry{name: "data/", typ: tar.TypeDir},
		entry{name: "data/old", body: "old\n"},
		entry{name: "swap/", typ: tar.TypeDir},
		entry{name: "swap/inside", body: "inside\n"},
		entry{name: long, body: "long\n"},
	)
	top := layer(t,
		entry{name: "etc/a", body: "changed\n", xattrs: map[string]string{"user.hangar": "yes"}},
		entry{name: "etc/.wh.gone"},
		entry{name: "opt/.wh.gonedir"},
		entry{name: "data/", typ: tar.TypeDir},
		entry{name: "data/.wh..wh..opq"},
		entry{name: "data/new", body: "new\n"},
		entry{name: "swap", body: "now a file\n"},
		entry{name: "etc/hard", typ: tar.TypeLink, link: "etc/a"},
		entry{name: "bin", typ: tar.TypeSymlink, link: "usr/bin"},
	)
	layers := [][]byte{top, bottom}
	next := func() (io.ReadCloser, error) {
		if len(layers) == 0 {
			return nil, nil
		}
		l := layers[0]
		layers = layers[1:]
		return io.NopCloser(bytes.NewReader(l)), nil
	}
	dir := t.TempDir()
	img := filepath.Join(dir, "image.erofs")
	if err := vm.BuildEROFS(context.Background(), img, next); err != nil {
		t.Fatal(err)
	}
	if st, err := os.Stat(img); err != nil || st.Size()%(2<<20) != 0 {
		t.Fatalf("the image: %v, %v", st, err)
	}
	tree := filepath.Join(dir, "tree")
	if out, err := exec.Command("fsck.erofs", "--extract="+tree, "--preserve-perms", img).CombinedOutput(); err != nil {
		t.Fatalf("extracting the image: %v: %s", err, out)
	}
	read := func(p string) string {
		b, err := os.ReadFile(filepath.Join(tree, p))
		if err != nil {
			return "<" + err.Error() + ">"
		}
		return string(b)
	}
	exists := func(p string) bool {
		_, err := os.Lstat(filepath.Join(tree, p))
		return err == nil
	}
	for p, want := range map[string]string{
		"etc/a": "changed\n", "etc/kept": "kept\n", "data/new": "new\n", "swap": "now a file\n",
		"etc/hard": "changed\n", long: "long\n",
	} {
		if got := read(p); got != want {
			t.Errorf("%s holds %q, want %q", p, got, want)
		}
	}
	for _, p := range []string{"etc/gone", "etc/.wh.gone", "opt/gonedir", "opt/.wh.gonedir", "data/old", "data/.wh..wh..opq", "swap/inside"} {
		if exists(p) {
			t.Errorf("%s is in the image", p)
		}
	}
	if target, err := os.Readlink(filepath.Join(tree, "bin")); err != nil || target != "usr/bin" {
		t.Errorf("bin links to %q (%v)", target, err)
	}
	for p, want := range map[string]os.FileMode{".": 0o755, "usr/bin/tool": 0o755, "etc/kept": 0o644} {
		st, err := os.Stat(filepath.Join(tree, p))
		if err != nil || st.Mode().Perm() != want {
			t.Errorf("%s has mode %v (%v), want %v", p, st.Mode().Perm(), err, want)
		}
	}
}

// An image whose layers never say what / is gets the root Docker would give
// it: root's, and 0755.
func TestBuildEROFSRoot(t *testing.T) {
	for _, tool := range []string{"mkfs.erofs", "fsck.erofs"} {
		if _, err := exec.LookPath(tool); err != nil {
			t.Skip(tool + " is not installed")
		}
	}
	l := layer(t, entry{name: "etc/", typ: tar.TypeDir}, entry{name: "etc/a", body: "a\n"})
	done := false
	next := func() (io.ReadCloser, error) {
		if done {
			return nil, nil
		}
		done = true
		return io.NopCloser(bytes.NewReader(l)), nil
	}
	dir := t.TempDir()
	img := filepath.Join(dir, "image.erofs")
	if err := vm.BuildEROFS(context.Background(), img, next); err != nil {
		t.Fatal(err)
	}
	tree := filepath.Join(dir, "tree")
	if out, err := exec.Command("fsck.erofs", "--extract="+tree, "--preserve-perms", img).CombinedOutput(); err != nil {
		t.Fatalf("extracting the image: %v: %s", err, out)
	}
	if st, err := os.Stat(tree); err != nil || st.Mode().Perm() != 0o755 {
		t.Errorf("/ has mode %v (%v), want 0755", st.Mode().Perm(), err)
	}
}
