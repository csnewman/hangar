//go:build linux

package nfs

import (
	"bytes"
	"crypto/rand"
	"encoding/binary"
	"errors"
	"path"
	"slices"
	"strings"
	"sync"

	"golang.org/x/sys/unix"
)

// A path here is relative to the server's root: "" is the root, which lists
// the sets; "<set>" a set's directory; "<set>/a/b" a file in it.

// split is a path's set, and the path in the set.
func split(p string) (set, rest string) {
	set, rest, _ = strings.Cut(p, "/")
	return set, rest
}

// visible reports why a path may not be reached, or nil: its set is not
// the environment's, or the file is hidden from it.
func (s *Server) visible(p string) uint32 {
	if p == "" {
		return nfsOK
	}
	set, rest := split(p)
	if !slices.Contains(s.view.Sets(), set) {
		return errNoEnt
	}
	if rest != "" && s.view.Hidden(set, rest) {
		return errNoEnt
	}
	return nfsOK
}

// validName reports whether a name is one component, as a LOOKUP or a
// create gives it.
func validName(n string) uint32 {
	switch {
	case n == "" || n == "." || n == "..":
		return errBadName
	case len(n) > maxName:
		return errNameTooLong
	case strings.ContainsAny(n, "/\x00"):
		return errBadName
	}
	return nfsOK
}

// openDir opens a directory of the root, beneath it and through no
// symbolic link: a link made in a set cannot lead a later call out of it.
// The descriptor is O_PATH, for the *at calls.
func (s *Server) openDir(p string) (int, error) {
	if p == "" {
		return unix.Dup(s.rootFD)
	}
	return unix.Openat2(s.rootFD, p, &unix.OpenHow{
		Flags:   unix.O_PATH | unix.O_DIRECTORY | unix.O_CLOEXEC,
		Resolve: unix.RESOLVE_BENEATH | unix.RESOLVE_NO_SYMLINKS | unix.RESOLVE_NO_MAGICLINKS,
	})
}

// at opens the directory a path is in, and gives its last component.
func (s *Server) at(p string) (int, string, error) {
	dir, name := path.Split(p)
	fd, err := s.openDir(strings.TrimSuffix(dir, "/"))
	return fd, name, err
}

// lstat is a file's attributes, without following a link.
func (s *Server) lstat(p string) (unix.Stat_t, error) {
	var st unix.Stat_t
	if p == "" {
		return st, errors.New("the root has no file")
	}
	fd, name, err := s.at(p)
	if err != nil {
		return st, err
	}
	defer unix.Close(fd)
	err = unix.Fstatat(fd, name, &st, unix.AT_SYMLINK_NOFOLLOW)
	return st, err
}

// openFile opens a file of the root, never through a link.
func (s *Server) openFile(p string, flags int, mode uint32) (int, error) {
	fd, name, err := s.at(p)
	if err != nil {
		return -1, err
	}
	defer unix.Close(fd)
	return unix.Openat(fd, name, flags|unix.O_NOFOLLOW|unix.O_CLOEXEC, mode)
}

// ensureSet makes a set's directory if it is not there yet: a set is the
// environment's from when it is given, whether or not it has files.
func (s *Server) ensureSet(set string) error {
	err := unix.Mkdirat(s.rootFD, set, 0o755)
	if errors.Is(err, unix.EEXIST) {
		return nil
	}
	return err
}

// errno is the NFS status for a system call's error.
func errno(err error) uint32 {
	var e unix.Errno
	if !errors.As(err, &e) {
		return errServerFault
	}
	switch e {
	case unix.EPERM:
		return errPerm
	case unix.ENOENT:
		return errNoEnt
	case unix.EIO:
		return errIO
	case unix.ENXIO:
		return errNXIO
	case unix.EACCES:
		return errAccess
	case unix.EEXIST:
		return errExist
	case unix.EXDEV:
		return errXDev
	case unix.ENOTDIR:
		return errNotDir
	case unix.EISDIR:
		return errIsDir
	case unix.EINVAL:
		return errInval
	case unix.EFBIG:
		return errFBig
	case unix.ENOSPC:
		return errNoSpc
	case unix.EROFS:
		return errROFS
	case unix.EMLINK:
		return errMLink
	case unix.ENAMETOOLONG:
		return errNameTooLong
	case unix.ENOTEMPTY:
		return errNotEmpty
	case unix.EDQUOT:
		return errDQuot
	case unix.ESTALE:
		return errStale
	case unix.ELOOP:
		// A link in the way of a path: openat2 refuses to follow it.
		return errSymlink
	case unix.EAGAIN:
		return errDelay
	case unix.EBADF:
		return errOpenMode
	}
	return errIO
}

// handles gives each path a filehandle, and finds it again by one. A
// handle is an ID and the server's boot verifier: one from before the
// server started is stale. The root's is always the same, so a client
// whose server started again finds every name again from there.
type handles struct {
	boot [8]byte

	mu    sync.Mutex
	next  uint64
	ids   map[string]uint64
	paths map[uint64]string
}

func newHandles() *handles {
	h := &handles{next: 2, ids: map[string]uint64{"": 1}, paths: map[uint64]string{1: ""}}
	rand.Read(h.boot[:])
	return h
}

// rootHandle is the root's handle, whichever server gave it.
var rootHandle = []byte("hangar-nfs-root\x00")

func (h *handles) handle(p string) []byte {
	if p == "" {
		return slices.Clone(rootHandle)
	}
	h.mu.Lock()
	id, ok := h.ids[p]
	if !ok {
		id = h.next
		h.next++
		h.ids[p] = id
		h.paths[id] = p
	}
	h.mu.Unlock()
	fh := make([]byte, 16)
	binary.BigEndian.PutUint64(fh, id)
	copy(fh[8:], h.boot[:])
	return fh
}

func (h *handles) path(fh []byte) (string, uint32) {
	if bytes.Equal(fh, rootHandle) {
		return "", nfsOK
	}
	if len(fh) != 16 {
		return "", errBadHandle
	}
	if !bytes.Equal(fh[8:], h.boot[:]) {
		return "", errStale
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	p, ok := h.paths[binary.BigEndian.Uint64(fh)]
	if !ok {
		return "", errStale
	}
	return p, nfsOK
}

// moved follows a rename: from, and everything under it, are at to.
func (h *handles) moved(from, to string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if id, ok := h.ids[to]; ok {
		delete(h.paths, id)
		delete(h.ids, to)
	}
	for p, id := range h.ids {
		var np string
		switch {
		case p == from:
			np = to
		case strings.HasPrefix(p, from+"/"):
			np = to + p[len(from):]
		default:
			continue
		}
		delete(h.ids, p)
		h.ids[np] = id
		h.paths[id] = np
	}
}

// removed forgets a path removed: its handle is stale.
func (h *handles) removed(p string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if id, ok := h.ids[p]; ok {
		delete(h.ids, p)
		delete(h.paths, id)
	}
}
