package erofs_test

import (
	"archive/tar"
	"bytes"
	"io"
	"maps"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"testing"

	"golang.org/x/sys/unix"

	"github.com/csnewman/hangar/internal/erofs"
)

func tarImage(t *testing.T) ([]want, []byte, *erofs.Image, string) {
	ws := tree()
	data := tarOf(t, ws, map[string]string{"usr/bin/sudoedit": "usr/bin/sudo"})
	im, p := build(t, func(w *erofs.Writer) error { return w.AddTar(tar.NewReader(bytes.NewReader(data))) })
	return ws, data, im, p
}

// The kernel mounts what Writer makes and sees what was put in it.
func TestKernelMounts(t *testing.T) {
	if os.Geteuid() != 0 {
		t.Skip("mounting needs root")
	}
	ws, _, im, p := tarImage(t)
	mnt := t.TempDir()
	if out, err := exec.Command("mount", "-t", "erofs", "-o", "loop,ro", p, mnt).CombinedOutput(); err != nil {
		t.Fatalf("mount: %v: %s", err, out)
	}
	defer exec.Command("umount", mnt).Run()

	for _, w := range ws {
		full := filepath.Join(mnt, w.path)
		var st unix.Stat_t
		if err := unix.Lstat(full, &st); err != nil {
			t.Errorf("%s: %v", w.path, err)
			continue
		}
		if st.Mode&0o7777 != w.perm || st.Uid != w.uid || st.Gid != w.gid {
			t.Errorf("%s: %o %d:%d, want %o %d:%d", w.path, st.Mode&0o7777, st.Uid, st.Gid, w.perm, w.uid, w.gid)
		}
		if !w.mtime.IsZero() && (st.Mtim.Sec != w.mtime.Unix() || st.Mtim.Nsec != int64(w.mtime.Nanosecond())) {
			t.Errorf("%s: modified %d.%d", w.path, st.Mtim.Sec, st.Mtim.Nsec)
		}
		switch w.typ {
		case erofs.TypeRegular:
			if got, err := os.ReadFile(full); err != nil || !bytes.Equal(got, w.data) {
				t.Errorf("%s: %d bytes (%v), want %d", w.path, len(got), err, len(w.data))
			}
		case erofs.TypeSymlink:
			if got, err := os.Readlink(full); err != nil || got != w.target {
				t.Errorf("%s: points to %d bytes (%v)", w.path, len(got), err)
			}
		case erofs.TypeCharDevice, erofs.TypeBlockDevice:
			if unix.Major(st.Rdev) != w.major || unix.Minor(st.Rdev) != w.minor {
				t.Errorf("%s: device %d:%d", w.path, unix.Major(st.Rdev), unix.Minor(st.Rdev))
			}
		}
		for name, v := range w.xattrs {
			buf := make([]byte, 70000)
			n, err := unix.Lgetxattr(full, name, buf)
			if err != nil || !bytes.Equal(buf[:n], v) {
				t.Errorf("%s: attribute %s: %v", w.path, name, err)
			}
		}
	}

	// Every directory lists what the reader lists, and a name the kernel
	// finds by its binary search.
	var walk func(rel string)
	walk = func(rel string) {
		in, err := im.Lookup(rel, false)
		if err != nil {
			t.Fatal(err)
		}
		ents, _ := in.ReadDir()
		var mine []string
		for _, e := range ents {
			mine = append(mine, e.Name)
			if e.Type == erofs.TypeDir {
				walk(filepath.Join(rel, e.Name))
			}
		}
		dirents, err := os.ReadDir(filepath.Join(mnt, rel))
		if err != nil {
			t.Fatal(err)
		}
		var theirs []string
		for _, d := range dirents {
			theirs = append(theirs, d.Name())
		}
		slices.Sort(mine)
		if !slices.Equal(mine, theirs) {
			t.Errorf("%s: the kernel lists %d entries, the reader %d", rel, len(theirs), len(mine))
		}
	}
	walk(".")
	var a, b unix.Stat_t
	unix.Lstat(filepath.Join(mnt, "usr/bin/sudo"), &a)
	unix.Lstat(filepath.Join(mnt, "usr/bin/sudoedit"), &b)
	if a.Ino != b.Ino || a.Nlink != 2 {
		t.Errorf("a hard link: inodes %d and %d, %d links", a.Ino, b.Ino, a.Nlink)
	}
}

// erofs-utils' own checker finds nothing wrong, reading every file.
func TestFsck(t *testing.T) {
	if _, err := exec.LookPath("fsck.erofs"); err != nil {
		t.Skip("no fsck.erofs")
	}
	_, _, _, p := tarImage(t)
	// --extract reads every file, and --xattrs every attribute.
	if out, err := exec.Command("fsck.erofs", "--extract", "--xattrs", p).CombinedOutput(); err != nil {
		t.Fatalf("fsck.erofs: %v: %s", err, out)
	}
}

// The reader reads mkfs.erofs's image of the same tree as it reads
// Writer's.
func TestMatchesMkfs(t *testing.T) {
	if _, err := exec.LookPath("mkfs.erofs"); err != nil {
		t.Skip("no mkfs.erofs")
	}
	_, data, ours, _ := tarImage(t)
	p := filepath.Join(t.TempDir(), "mkfs.erofs")
	cmd := exec.Command("mkfs.erofs", "--quiet", "--tar=f", "-b", "4096", p)
	cmd.Stdin = bytes.NewReader(data)
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("mkfs.erofs: %v: %s", err, out)
	}
	f, err := os.Open(p)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	theirs, err := erofs.Open(f)
	if err != nil {
		t.Fatal(err)
	}
	var walk func(rel string)
	walk = func(rel string) {
		a, err := ours.Lookup(rel, false)
		if err != nil {
			t.Fatal(err)
		}
		b, err := theirs.Lookup(rel, false)
		if err != nil {
			t.Errorf("%s: mkfs.erofs's: %v", rel, err)
			return
		}
		if a.Type != b.Type || a.Perm != b.Perm || a.UID != b.UID || a.GID != b.GID || a.Size != b.Size ||
			a.Nlink != b.Nlink || a.Major != b.Major || a.Minor != b.Minor || a.ModTime.Unix() != b.ModTime.Unix() {
			t.Errorf("%s: ours %+v, mkfs.erofs's %+v", rel, *a, *b)
		}
		ax, _ := a.Xattrs()
		bx, err := b.Xattrs()
		if err != nil || !maps.EqualFunc(ax, bx, bytes.Equal) {
			t.Errorf("%s: attributes differ (%v)", rel, err)
		}
		if a.Type == erofs.TypeRegular || a.Type == erofs.TypeSymlink {
			ad, _ := io.ReadAll(a.Data())
			bd, err := io.ReadAll(b.Data())
			if err != nil || !bytes.Equal(ad, bd) {
				t.Errorf("%s: contents differ (%v)", rel, err)
			}
		}
		if a.Type != erofs.TypeDir {
			return
		}
		ae, _ := a.ReadDir()
		be, _ := b.ReadDir()
		if len(ae) != len(be) {
			t.Errorf("%s: %d entries, mkfs.erofs's %d", rel, len(ae), len(be))
		}
		for _, e := range ae {
			walk(filepath.Join(rel, e.Name))
		}
	}
	walk(".")
}
