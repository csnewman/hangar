package erofs

import (
	"bytes"
	"errors"

	"golang.org/x/sys/unix"
)

// readXattrs is a file's extended attributes, not following a symlink.
func readXattrs(p string) (map[string][]byte, error) {
	size, err := unix.Llistxattr(p, nil)
	if err != nil {
		if errors.Is(err, unix.ENOTSUP) {
			return nil, nil
		}
		return nil, err
	}
	if size == 0 {
		return nil, nil
	}
	list := make([]byte, size)
	if size, err = unix.Llistxattr(p, list); err != nil {
		return nil, err
	}
	out := map[string][]byte{}
	for name := range bytes.SplitSeq(list[:size], []byte{0}) {
		if len(name) == 0 {
			continue
		}
		n, err := unix.Lgetxattr(p, string(name), nil)
		if err != nil {
			if errors.Is(err, unix.ENODATA) {
				continue
			}
			return nil, err
		}
		v := make([]byte, n)
		if n, err = unix.Lgetxattr(p, string(name), v); err != nil {
			return nil, err
		}
		out[string(name)] = v[:n]
	}
	return out, nil
}
