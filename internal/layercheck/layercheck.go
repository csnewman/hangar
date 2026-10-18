// Package layercheck finds what an environment's writable layer would hide
// of an image upgrade: the overlay's upper directory is kept across it, so a
// file the layer replaced or deleted stays as the layer has it, whatever the
// newer image changed there. It reads the layer and both images without
// mounting them: the layer from its ext4 disk (internal/ext4), the images
// from their EROFS files (internal/erofs).
package layercheck

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"slices"

	"github.com/csnewman/hangar/internal/erofs"
	"github.com/csnewman/hangar/internal/ext4"
)

// Layer is an overlay's upper directory as the check reads it: its tree, in
// which a whiteout is a character device numbered 0:0, and whether
// overlayfs takes a directory as opaque, hiding everything beneath it.
type Layer interface {
	fs.FS
	Opaque(dir string) (bool, error)
}

// PackageDatabases are where package managers record what is installed. A
// copy in an environment's writable layer describes the image it was made
// over plus what was installed on it, and hides the newer image's own.
var PackageDatabases = []string{
	"var/lib/dpkg/status",
	"var/lib/rpm/rpmdb.sqlite",
	"var/lib/rpm/Packages",
	"usr/lib/sysimage/rpm/rpmdb.sqlite",
}

// ignored are left out of the comparison: scratch space, logs and caches,
// which nothing depends on, and the stamps systemd compares with /usr to
// decide whether to redo its post-update work (ldconfig among it), which an
// older stamp over a newer image makes it do.
var ignored = []string{"tmp", "var/tmp", "var/log", "var/cache", "etc/.updated", "var/.updated"}

// Result is what a layer would hide.
type Result struct {
	// Conflicts are the paths, from /, the layer replaces or deletes that
	// the images differ in, and the directories it makes opaque whose
	// entries they differ in, which end in "/".
	Conflicts []string
	// Packages is whether one of them is a package manager's database.
	Packages bool
}

// Compare lists what in layer would hide a change between two copies of the
// image beneath it. What the layer adds that neither copy has, or changes
// that both copies have alike, hides nothing new.
func Compare(layer Layer, older, newer *erofs.Image) (Result, error) {
	var r Result
	err := fs.WalkDir(layer, ".", func(rel string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if rel == "." {
			return nil
		}
		if slices.Contains(ignored, rel) {
			if d.IsDir() {
				return fs.SkipDir
			}
			return nil
		}
		var conflict bool
		if d.IsDir() {
			opaque, err := layer.Opaque(rel)
			if err != nil {
				return fmt.Errorf("%s: %w", rel, err)
			}
			if !opaque {
				return nil
			}
			conflict, err = entriesDiffer(older, newer, rel)
			if err != nil {
				return fmt.Errorf("%s: %w", rel, err)
			}
			if conflict {
				r.Conflicts = append(r.Conflicts, "/"+rel+"/")
			}
			return nil
		}
		// A whiteout, which deletes the file beneath, is compared like
		// anything else: what matters is whether the copies differ in the
		// path it hides.
		conflict, err = pathsDiffer(older, newer, rel)
		if err != nil {
			return fmt.Errorf("%s: %w", rel, err)
		}
		if conflict {
			r.Conflicts = append(r.Conflicts, "/"+rel)
			if slices.Contains(PackageDatabases, rel) {
				r.Packages = true
			}
		}
		return nil
	})
	return r, err
}

// lookup finds rel in an image, resolving it as the guest would -- an
// absolute symlink stays inside the image -- without following its last
// component. It is nil for a path that is not there.
func lookup(im *erofs.Image, rel string) (*erofs.Inode, error) {
	in, err := im.Lookup(rel, false)
	if errors.Is(err, fs.ErrNotExist) {
		return nil, nil
	}
	return in, err
}

// pathsDiffer is whether the two images differ at rel: one has it and the
// other not, or they have different kinds of file, modes, owners, targets
// or contents.
func pathsDiffer(older, newer *erofs.Image, rel string) (bool, error) {
	a, err := lookup(older, rel)
	if err != nil {
		return false, err
	}
	b, err := lookup(newer, rel)
	if err != nil {
		return false, err
	}
	if a == nil || b == nil {
		return (a == nil) != (b == nil), nil
	}
	if a.Type != b.Type || a.Perm != b.Perm || a.UID != b.UID || a.GID != b.GID {
		return true, nil
	}
	switch a.Type {
	case erofs.TypeSymlink:
		ta, err := a.Readlink()
		if err != nil {
			return false, err
		}
		tb, err := b.Readlink()
		if err != nil {
			return false, err
		}
		return ta != tb, nil
	case erofs.TypeRegular:
		if a.Size != b.Size {
			return true, nil
		}
		return contentsDiffer(a.Data(), b.Data())
	case erofs.TypeCharDevice, erofs.TypeBlockDevice:
		return a.Major != b.Major || a.Minor != b.Minor, nil
	}
	return false, nil
}

func contentsDiffer(a, b io.Reader) (bool, error) {
	ba, bb := make([]byte, 64<<10), make([]byte, 64<<10)
	for {
		na, ea := io.ReadFull(a, ba)
		nb, eb := io.ReadFull(b, bb)
		if na != nb || !bytes.Equal(ba[:na], bb[:nb]) {
			return true, nil
		}
		aDone := ea == io.EOF || ea == io.ErrUnexpectedEOF
		bDone := eb == io.EOF || eb == io.ErrUnexpectedEOF
		if aDone || bDone {
			return aDone != bDone, nil
		}
		if ea != nil {
			return false, ea
		}
		if eb != nil {
			return false, eb
		}
	}
}

// entriesDiffer is whether the two images' directories at rel hold
// different names, or one has the directory and the other not.
func entriesDiffer(older, newer *erofs.Image, rel string) (bool, error) {
	names := func(im *erofs.Image) ([]string, bool, error) {
		in, err := lookup(im, rel)
		if err != nil || in == nil || in.Type != erofs.TypeDir {
			return nil, false, err
		}
		ents, err := in.ReadDir()
		if err != nil {
			return nil, false, err
		}
		out := make([]string, len(ents))
		for i, e := range ents {
			out[i] = e.Name
		}
		slices.Sort(out)
		return out, true, nil
	}
	a, hasA, err := names(older)
	if err != nil {
		return false, err
	}
	b, hasB, err := names(newer)
	if err != nil {
		return false, err
	}
	return hasA != hasB || !slices.Equal(a, b), nil
}

// Ext4 is the upper directory at dir of an ext4 filesystem, the disk an
// environment's writable layer is kept on.
func Ext4(f *ext4.FS, dir string) (Layer, error) {
	sub, err := fs.Sub(f.FS(), dir)
	if err != nil {
		return nil, err
	}
	return ext4Layer{FS: sub, f: f, dir: dir}, nil
}

type ext4Layer struct {
	fs.FS
	f   *ext4.FS
	dir string
}

// Opaque reads overlayfs's mark, trusted.overlay.opaque set to "y".
func (l ext4Layer) Opaque(rel string) (bool, error) {
	in, err := l.f.Lookup(l.dir+"/"+rel, false)
	if err != nil {
		return false, err
	}
	attrs, err := in.Xattrs()
	if err != nil {
		return false, err
	}
	return string(attrs["trusted.overlay.opaque"]) == "y", nil
}
