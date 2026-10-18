package vm

import (
	"archive/tar"
	"context"
	"errors"
	"io"
	"os"
	"path"
	"strings"

	"github.com/csnewman/hangar/internal/erofs"
)

// An image's layers are tar archives, each recording what it changes over
// the layers beneath it: files it adds or replaces, and with OCI whiteouts
// (.wh.<name>, and .wh..wh..opq for a directory) what it removes. They are
// merged here into the one tree they describe, as a single tar stream, top
// layer first: a path's first entry is the one that stands, and a whiteout,
// an opaque directory, or a non-directory replacing a directory hides what
// the layers beneath had there. Nothing is unpacked: each entry goes straight
// into the image, which accepts a file before its directory.

// layerMerge is what the layers above have said, for the ones beneath.
type layerMerge struct {
	seen      map[string]bool
	nondir    map[string]bool
	whiteouts map[string]bool
	opaque    map[string]bool
}

func newLayerMerge() *layerMerge {
	return &layerMerge{seen: map[string]bool{}, nondir: map[string]bool{},
		whiteouts: map[string]bool{}, opaque: map[string]bool{}}
}

// clean is a tar entry's path as the merge keys it: relative, without "./"
// or a trailing slash. The root is ".".
func clean(name string) string {
	p := path.Clean("/" + name)
	if p == "/" {
		return "."
	}
	return p[1:]
}

// hidden is whether a layer above removed or replaced p.
func (m *layerMerge) hidden(p string) bool {
	for q := p; q != "."; q = path.Dir(q) {
		if m.whiteouts[q] {
			return true
		}
		if q != p && (m.opaque[q] || m.nondir[q]) {
			return true
		}
	}
	return false
}

// layer adds one layer's entries, beneath those already added, to w.
func (m *layerMerge) layer(r *tar.Reader, w *erofs.Writer) error {
	var whiteouts, opaque []string
	for {
		hdr, err := r.Next()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return err
		}
		p := clean(hdr.Name)
		dir, base := path.Dir(p), path.Base(p)
		if base == ".wh..wh..opq" {
			opaque = append(opaque, dir)
			continue
		}
		if name, ok := strings.CutPrefix(base, ".wh."); ok {
			whiteouts = append(whiteouts, path.Join(dir, name))
			continue
		}
		// A path a layer above has decided is not this layer's to give.
		if m.seen[p] || m.hidden(p) {
			continue
		}
		m.seen[p] = true
		if hdr.Typeflag != tar.TypeDir {
			m.nondir[p] = true
		}
		hdr.Name = p
		if hdr.Typeflag == tar.TypeLink {
			hdr.Linkname = clean(hdr.Linkname)
		}
		if err := w.AddTarEntry(hdr, r); err != nil {
			return err
		}
	}
	// A layer's whiteouts and opaque directories hide only what is beneath
	// it, not its own entries.
	for _, p := range whiteouts {
		m.whiteouts[p] = true
	}
	for _, p := range opaque {
		m.opaque[p] = true
	}
	return nil
}

// BuildEROFS writes the tree an image's layers describe to out as an EROFS
// image (internal/erofs), padded to a multiple of 2 MiB for persistent
// memory. next yields the layers' uncompressed tar streams top first, and
// nil after the last. An image whose layers never say what / is gets the
// root Docker gives it: root's, and 0755.
func BuildEROFS(ctx context.Context, out string, next func() (io.ReadCloser, error)) error {
	f, err := os.Create(out)
	if err != nil {
		return err
	}
	err = func() error {
		w := erofs.NewWriter(f)
		m := newLayerMerge()
		for {
			if err := ctx.Err(); err != nil {
				return err
			}
			rc, err := next()
			if err != nil {
				return err
			}
			if rc == nil {
				break
			}
			err = m.layer(tar.NewReader(rc), w)
			rc.Close()
			if err != nil {
				return err
			}
		}
		return w.Close()
	}()
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	if err == nil {
		err = padEROFS(out)
	}
	if err != nil {
		os.Remove(out)
	}
	return err
}

// padEROFS rounds an image's size up to what a persistent-memory region is
// aligned to.
func padEROFS(path string) error {
	st, err := os.Stat(path)
	if err != nil {
		return err
	}
	if rem := st.Size() % erofsAlign; rem != 0 {
		return os.Truncate(path, st.Size()+erofsAlign-rem)
	}
	return nil
}
