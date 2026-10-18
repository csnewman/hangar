package erofs

import (
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"syscall"

	"golang.org/x/sys/unix"
)

// AddDir adds the tree under root, which becomes the image's root, as it
// is on disk: owners, modes, times, hard links and extended attributes.
func (w *Writer) AddDir(root string) error {
	type fileID struct{ dev, ino uint64 }
	seen := map[fileID]string{}
	return filepath.WalkDir(root, func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(root, p)
		if err != nil {
			return err
		}
		rel = filepath.ToSlash(rel)
		info, err := d.Info()
		if err != nil {
			return err
		}
		st, ok := info.Sys().(*syscall.Stat_t)
		if !ok {
			return fmt.Errorf("%s: no owner to read", p)
		}
		e := Entry{
			Path:    rel,
			Perm:    uint32(st.Mode) & 0o7777,
			UID:     st.Uid,
			GID:     st.Gid,
			ModTime: info.ModTime(),
		}
		if e.Xattrs, err = readXattrs(p); err != nil {
			return fmt.Errorf("%s: %w", p, err)
		}
		mode := info.Mode()
		if !mode.IsDir() && st.Nlink > 1 {
			id := fileID{uint64(st.Dev), uint64(st.Ino)}
			if first, ok := seen[id]; ok {
				return w.Link(rel, first)
			}
			seen[id] = rel
		}
		switch {
		case mode.IsRegular():
			e.Type, e.Size = TypeRegular, info.Size()
			f, err := os.Open(p)
			if err != nil {
				return err
			}
			defer f.Close()
			return w.Add(e, f)
		case mode.IsDir():
			e.Type = TypeDir
		case mode&fs.ModeSymlink != 0:
			if e.Target, err = os.Readlink(p); err != nil {
				return err
			}
			e.Type = TypeSymlink
		case mode&fs.ModeCharDevice != 0:
			e.Type = TypeCharDevice
			e.Major, e.Minor = unix.Major(uint64(st.Rdev)), unix.Minor(uint64(st.Rdev))
		case mode&fs.ModeDevice != 0:
			e.Type = TypeBlockDevice
			e.Major, e.Minor = unix.Major(uint64(st.Rdev)), unix.Minor(uint64(st.Rdev))
		case mode&fs.ModeNamedPipe != 0:
			e.Type = TypeFIFO
		case mode&fs.ModeSocket != 0:
			e.Type = TypeSocket
		default:
			return fmt.Errorf("%s: a file of mode %v", p, mode)
		}
		return w.Add(e, nil)
	})
}
