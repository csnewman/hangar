package ext4_test

import (
	"bytes"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"syscall"
	"testing"
	"testing/fstest"

	"golang.org/x/sys/unix"

	"github.com/csnewman/hangar/internal/ext4"
)

// mkfs finds mkfs.ext4: on PATH, or the one dev/krun-build.sh builds on a
// Mac.
func mkfs(t *testing.T) string {
	t.Helper()
	if p, err := exec.LookPath("mkfs.ext4"); err == nil {
		return p
	}
	for _, p := range []string{"/sbin/mkfs.ext4", "/usr/sbin/mkfs.ext4", "../../out/krun/bin/mkfs.ext4"} {
		if _, err := os.Stat(p); err == nil {
			abs, _ := filepath.Abs(p)
			return abs
		}
	}
	t.Skip("no mkfs.ext4")
	return ""
}

func pattern(n int, seed byte) []byte {
	b := make([]byte, n)
	for i := range b {
		b[i] = byte(i*13+i/1000) ^ seed
	}
	return b
}

// source makes a tree to put in a filesystem.
func source(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	write := func(p string, data []byte, mode os.FileMode) {
		full := filepath.Join(dir, p)
		os.MkdirAll(filepath.Dir(full), 0o755)
		if err := os.WriteFile(full, data, mode); err != nil {
			t.Fatal(err)
		}
		os.Chmod(full, mode)
	}
	write("etc/empty", nil, 0o644)
	write("etc/small", []byte("small\n"), 0o600)
	write("etc/block", pattern(4096, 1), 0o644)
	write("etc/big", pattern(5<<20+33, 2), 0o755)
	write("usr/bin/sudo", []byte("sudo"), 0o4755)
	os.MkdirAll(filepath.Join(dir, "tmp"), 0o755)
	os.Chmod(filepath.Join(dir, "tmp"), 0o1777)
	os.Symlink("../usr/bin/sudo", filepath.Join(dir, "etc/short-link"))
	os.Symlink(strings.Repeat("./", 60)+"small", filepath.Join(dir, "etc/long-link"))
	os.Symlink("/etc/small", filepath.Join(dir, "abs-link"))
	os.Link(filepath.Join(dir, "etc/small"), filepath.Join(dir, "etc/hard"))
	// Enough entries to need several blocks, and a hashed index.
	for i := range 1500 {
		write(fmt.Sprintf("many/entry-%04d-%s", i, strings.Repeat("x", i%30)), []byte(fmt.Sprint(i)), 0o644)
	}
	unix.Mkfifo(filepath.Join(dir, "etc/fifo"), 0o644)
	return dir
}

// image makes a filesystem of dir with mkfs.ext4's options.
func image(t *testing.T, dir string, opts ...string) *ext4.FS {
	t.Helper()
	img := filepath.Join(t.TempDir(), "fs.ext4")
	f, err := os.Create(img)
	if err != nil {
		t.Fatal(err)
	}
	f.Truncate(256 << 20)
	f.Close()
	args := append([]string{"-q", "-F", "-d", dir}, opts...)
	if out, err := exec.Command(mkfs(t), append(args, img)...).CombinedOutput(); err != nil {
		t.Fatalf("mkfs.ext4 %v: %v: %s", opts, err, out)
	}
	g, err := os.Open(img)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { g.Close() })
	fsys, err := ext4.Open(g)
	if err != nil {
		t.Fatal(err)
	}
	return fsys
}

// check compares the filesystem with the tree it was made of.
func check(t *testing.T, fsys *ext4.FS, dir string) {
	t.Helper()
	err := filepath.WalkDir(dir, func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, _ := filepath.Rel(dir, p)
		if rel == "." || rel == "lost+found" {
			return nil
		}
		st, _ := os.Lstat(p)
		in, err := fsys.Lookup(rel, false)
		if err != nil {
			t.Errorf("%s: %v", rel, err)
			return nil
		}
		if want := st.Mode(); in.Mode != want {
			t.Errorf("%s: mode %v, want %v", rel, in.Mode, st.Mode())
		}
		sys := st.Sys().(*syscall.Stat_t)
		if in.UID != sys.Uid || in.GID != sys.Gid {
			t.Errorf("%s: owner %d:%d, want %d:%d", rel, in.UID, in.GID, sys.Uid, sys.Gid)
		}
		switch {
		case st.Mode().IsRegular():
			want, _ := os.ReadFile(p)
			r, err := in.Data()
			if err != nil {
				t.Errorf("%s: %v", rel, err)
				return nil
			}
			got, err := io.ReadAll(r)
			if err != nil || !bytes.Equal(got, want) {
				t.Errorf("%s: %d bytes (%v), want %d", rel, len(got), err, len(want))
			}
		case st.Mode()&fs.ModeSymlink != 0:
			want, _ := os.Readlink(p)
			if got, err := in.Readlink(); err != nil || got != want {
				t.Errorf("%s: points to %q (%v), want %q", rel, got, err, want)
			}
		case st.IsDir():
			ents, err := in.ReadDir()
			if err != nil {
				t.Errorf("%s: %v", rel, err)
				return nil
			}
			var got []string
			for _, e := range ents {
				got = append(got, e.Name)
			}
			slices.Sort(got)
			src, _ := os.ReadDir(p)
			var wantNames []string
			for _, e := range src {
				wantNames = append(wantNames, e.Name())
			}
			if !slices.Equal(got, wantNames) {
				t.Errorf("%s: %d entries, want %d", rel, len(got), len(wantNames))
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	a, _ := fsys.Lookup("etc/hard", false)
	b, _ := fsys.Lookup("etc/small", false)
	if a == nil || b == nil || a.Ino != b.Ino || a.Nlink != 2 {
		t.Errorf("a hard link is not one inode")
	}
	if in, err := fsys.Lookup("abs-link", true); err != nil || in.Ino != b.Ino {
		t.Errorf("an absolute link does not resolve in the filesystem's root: %v", err)
	}
	if _, err := fsys.Lookup("etc/small/x", false); err == nil {
		t.Error("a path through a file was found")
	}
}

func TestFeatures(t *testing.T) {
	dir := source(t)
	for name, opts := range map[string][]string{
		"defaults":        nil,
		"32-bit":          {"-O", "^64bit"},
		"1 KiB blocks":    {"-b", "1024"},
		"inline data":     {"-O", "inline_data"},
		"no dir index":    {"-O", "^dir_index"},
		"128-byte inodes": {"-I", "128"},
	} {
		t.Run(name, func(t *testing.T) { check(t, image(t, dir, opts...), dir) })
	}
}

func TestFS(t *testing.T) {
	dir := source(t)
	fsys := image(t, dir)
	if err := fstest.TestFS(fsys.FS(), "etc/small", "etc/big", "usr/bin/sudo", "many/entry-0007-xxxxxxx"); err != nil {
		t.Error(err)
	}
	b, err := fs.ReadFile(fsys.FS(), "etc/short-link")
	if err != nil || string(b) != "sudo" {
		t.Errorf("reading through a link: %q %v", b, err)
	}
}

// What overlayfs marks in an upper directory: a whiteout, a character
// device 0:0, and an opaque directory, marked by a trusted attribute. Both
// need root to make.
func TestOverlayMarks(t *testing.T) {
	if runtime.GOOS != "linux" || os.Geteuid() != 0 {
		t.Skip("making whiteouts and trusted attributes needs root on Linux")
	}
	dir := t.TempDir()
	os.MkdirAll(filepath.Join(dir, "upper/opaque"), 0o755)
	if err := unix.Mknod(filepath.Join(dir, "upper/gone"), unix.S_IFCHR|0o600, 0); err != nil {
		t.Fatal(err)
	}
	if err := unix.Setxattr(filepath.Join(dir, "upper/opaque"), "trusted.overlay.opaque", []byte("y"), 0); err != nil {
		t.Fatal(err)
	}
	unix.Setxattr(filepath.Join(dir, "upper/opaque"), "user.big", bytes.Repeat([]byte("v"), 3000), 0)
	unix.Mknod(filepath.Join(dir, "upper/tty"), unix.S_IFCHR|0o620, int(unix.Mkdev(4, 300)))
	fsys := image(t, dir)

	gone, err := fsys.Lookup("upper/gone", false)
	if err != nil || gone.Mode&fs.ModeCharDevice == 0 || gone.Major != 0 || gone.Minor != 0 {
		t.Errorf("the whiteout: %+v %v", gone, err)
	}
	tty, err := fsys.Lookup("upper/tty", false)
	if err != nil || tty.Major != 4 || tty.Minor != 300 {
		t.Errorf("a device: %+v %v", tty, err)
	}
	op, err := fsys.Lookup("upper/opaque", false)
	if err != nil {
		t.Fatal(err)
	}
	attrs, err := op.Xattrs()
	if err != nil || string(attrs["trusted.overlay.opaque"]) != "y" || len(attrs["user.big"]) != 3000 {
		t.Errorf("attributes %v (%v)", attrs, err)
	}
}
