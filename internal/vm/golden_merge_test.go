package vm_test

import (
	"archive/tar"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"flag"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/vm"
)

// The layers and the merged image's hash the Rust merge (crates/images) is
// held to as well: the same layers must make the same image, byte for
// byte. -update writes them from this merge.
const goldenLayers = "../../crates/images/tests/data/"

var update = flag.Bool("update", false, "write the golden layers and the merged image's hash from this merge")

func goldenLayer(t *testing.T, es ...tar.Header) []byte {
	t.Helper()
	var buf bytes.Buffer
	w := tar.NewWriter(&buf)
	for _, h := range es {
		body := ""
		if h.Typeflag == tar.TypeReg {
			body = h.Linkname
			h.Linkname = ""
			h.Size = int64(len(body))
		}
		h.Format = tar.FormatPAX
		if err := w.WriteHeader(&h); err != nil {
			t.Fatal(err)
		}
		io.WriteString(w, body)
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func TestGoldenMerge(t *testing.T) {
	at := time.Unix(1700000000, 0)
	file := func(name, body string, mode int64) tar.Header {
		// A regular file's body rides in Linkname until goldenLayer writes it.
		return tar.Header{Name: name, Typeflag: tar.TypeReg, Mode: mode, Linkname: body, ModTime: at}
	}
	dir := func(name string) tar.Header {
		return tar.Header{Name: name, Typeflag: tar.TypeDir, Mode: 0o755, ModTime: at}
	}
	long := strings.Repeat("d", 120) + "/" + strings.Repeat("f", 120)
	if *update {
		bigOwner := file("etc/owned", "owned\n", 0o600)
		bigOwner.Uid, bigOwner.Gid = 3000000, 3000001
		precise := file("etc/precise", "precise\n", 0o644)
		precise.ModTime = time.Unix(1700000000, 123456789)
		attrs := file("etc/a", "changed\n", 0o644)
		attrs.PAXRecords = map[string]string{"SCHILY.xattr.user.hangar": "yes", "SCHILY.xattr.security.selinux": "x"}
		bottom := goldenLayer(t,
			dir("./"), dir("etc/"),
			file("usr/bin/tool", "tool\n", 0o755), file("etc/a", "base\n", 0o644),
			file("etc/kept", "kept\n", 0o644), file("etc/gone", "gone\n", 0o644),
			dir("opt/gonedir/"), file("opt/gonedir/f", "f\n", 0o644),
			dir("data/"), file("data/old", "old\n", 0o644),
			dir("swap/"), file("swap/inside", "inside\n", 0o644),
			file(long, "long\n", 0o644), bigOwner, precise,
			tar.Header{Name: "dev/null", Typeflag: tar.TypeChar, Mode: 0o666, Devmajor: 1, Devminor: 3, ModTime: at},
			tar.Header{Name: "run/fifo", Typeflag: tar.TypeFifo, Mode: 0o644, ModTime: at},
		)
		top := goldenLayer(t,
			attrs, file("etc/.wh.gone", "", 0o644), file("opt/.wh.gonedir", "", 0o644),
			dir("data/"), file("data/.wh..wh..opq", "", 0o644), file("data/new", "new\n", 0o644),
			file("swap", "now a file\n", 0o644),
			tar.Header{Name: "etc/hard", Typeflag: tar.TypeLink, Linkname: "etc/a", ModTime: at},
			tar.Header{Name: "bin", Typeflag: tar.TypeSymlink, Linkname: "usr/bin", Mode: 0o777, ModTime: at},
		)
		os.WriteFile(goldenLayers+"bottom.tar", bottom, 0o644)
		os.WriteFile(goldenLayers+"top.tar", top, 0o644)
	}
	layers := []string{"top.tar", "bottom.tar"}
	next := func() (io.ReadCloser, error) {
		if len(layers) == 0 {
			return nil, nil
		}
		f, err := os.Open(goldenLayers + layers[0])
		layers = layers[1:]
		return f, err
	}
	img := filepath.Join(t.TempDir(), "image.erofs")
	if err := vm.BuildEROFS(context.Background(), img, next); err != nil {
		t.Fatal(err)
	}
	b, err := os.ReadFile(img)
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(b)
	got := hex.EncodeToString(sum[:]) + "\n"
	if *update {
		os.WriteFile(goldenLayers+"merged.sha256", []byte(got), 0o644)
		return
	}
	want, err := os.ReadFile(goldenLayers + "merged.sha256")
	if err != nil {
		t.Fatalf("%v (go test -run TestGoldenMerge -update writes it)", err)
	}
	if got != string(want) {
		t.Fatalf("the merged image's hash is %s, want %s", got, want)
	}
}
