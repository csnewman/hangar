package vm

import (
	"archive/tar"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path"
	"strings"
)

// An image's layers are tar archives, each recording what it changes over
// the layers beneath it: files it adds or replaces, and with OCI whiteouts
// (.wh.<name>, and .wh..wh..opq for a directory) what it removes. They are
// merged here into the one tree they describe, as a single tar stream, top
// layer first: a path's first entry is the one that stands, and a whiteout,
// an opaque directory, or a non-directory replacing a directory hides what
// the layers beneath had there. Nothing is unpacked: the stream goes
// straight into mkfs.erofs, which accepts a file before its directory.

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
func (m *layerMerge) layer(r *tar.Reader, w *tar.Writer) error {
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
		if hdr.Typeflag == tar.TypeDir {
			hdr.Name += "/"
		}
		if p == "." {
			// The root: its owner and mode are the image's /.
			hdr.Name = "./"
		}
		if hdr.Typeflag == tar.TypeLink {
			hdr.Linkname = clean(hdr.Linkname)
		}
		// PAX keeps extended attributes and long names as they came.
		hdr.Format = tar.FormatPAX
		if err := w.WriteHeader(hdr); err != nil {
			return err
		}
		if hdr.Typeflag == tar.TypeReg {
			if _, err := io.Copy(w, r); err != nil {
				return err
			}
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
// image, uncompressed with 4 KiB blocks, so a guest can map its files
// directly, padded to a multiple of 2 MiB for persistent memory. next yields
// the layers' uncompressed tar streams top first, and nil after the last.
// It needs mkfs.erofs (erofs-utils).
func BuildEROFS(ctx context.Context, out string, next func() (io.ReadCloser, error)) error {
	cmd := exec.CommandContext(ctx, "mkfs.erofs", "--quiet", "--tar=f", "-b", "4096", out)
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return err
	}
	var stderr strings.Builder
	cmd.Stderr = &stderr
	if err := cmd.Start(); err != nil {
		return err
	}
	werr := func() error {
		w := tar.NewWriter(stdin)
		m := newLayerMerge()
		for {
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
		if !m.seen["."] {
			// No layer says what / is, as many do not: it is root's, and
			// 0755, as Docker makes it. An entry after what it holds still
			// sets its owner and mode.
			hdr := &tar.Header{Name: "./", Typeflag: tar.TypeDir, Mode: 0o755, Format: tar.FormatPAX}
			if err := w.WriteHeader(hdr); err != nil {
				return err
			}
		}
		return w.Close()
	}()
	stdin.Close()
	if err := cmd.Wait(); err != nil {
		os.Remove(out)
		if werr != nil {
			return werr
		}
		return fmt.Errorf("mkfs.erofs: %w: %s", err, lastLineOf([]byte(stderr.String())))
	}
	if werr != nil {
		os.Remove(out)
		return werr
	}
	return padEROFS(out)
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
