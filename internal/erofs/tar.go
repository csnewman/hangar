package erofs

import (
	"archive/tar"
	"errors"
	"fmt"
	"io"
	"strings"
)

// AddTar adds every entry of a tar archive. Extended attributes come from
// its PAX records (SCHILY.xattr.*); a hard link's target must come before
// it, and a path may appear once, but for a directory made for what came
// before it.
func (w *Writer) AddTar(r *tar.Reader) error {
	for {
		hdr, err := r.Next()
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return err
		}
		if err := w.AddTarEntry(hdr, r); err != nil {
			return err
		}
	}
}

// AddTarEntry adds one tar entry, reading a regular file's data from r.
func (w *Writer) AddTarEntry(hdr *tar.Header, r io.Reader) error {
	e := Entry{
		Path:    hdr.Name,
		Perm:    uint32(hdr.Mode) & 0o7777,
		UID:     uint32(hdr.Uid),
		GID:     uint32(hdr.Gid),
		ModTime: hdr.ModTime,
		Major:   uint32(hdr.Devmajor),
		Minor:   uint32(hdr.Devminor),
	}
	for k, v := range hdr.PAXRecords {
		if name, ok := strings.CutPrefix(k, "SCHILY.xattr."); ok {
			if e.Xattrs == nil {
				e.Xattrs = map[string][]byte{}
			}
			e.Xattrs[name] = []byte(v)
		}
	}
	switch hdr.Typeflag {
	case tar.TypeReg, tar.TypeGNUSparse:
		e.Type, e.Size = TypeRegular, hdr.Size
		return w.Add(e, r)
	case tar.TypeDir:
		e.Type = TypeDir
	case tar.TypeSymlink:
		e.Type, e.Target = TypeSymlink, hdr.Linkname
	case tar.TypeLink:
		return w.Link(hdr.Name, hdr.Linkname)
	case tar.TypeChar:
		e.Type = TypeCharDevice
	case tar.TypeBlock:
		e.Type = TypeBlockDevice
	case tar.TypeFifo:
		e.Type = TypeFIFO
	case tar.TypeXGlobalHeader:
		return nil
	default:
		return fmt.Errorf("%s: an entry of tar type %q", hdr.Name, hdr.Typeflag)
	}
	return w.Add(e, nil)
}
