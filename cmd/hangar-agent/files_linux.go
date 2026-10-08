//go:build linux

package main

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/user"
	"path"
	"path/filepath"
	"strconv"
	"sync"
	"unsafe"

	"golang.org/x/sys/unix"

	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/vsock"
)

// sharedDir is where the files the environment shares with others are
// mounted: the worker serves them over NFS, on vsock, each copy a
// directory named by its ID.
const sharedDir = "/run/hangar/files"

// agentState is where the agent keeps, on the environment's own disk,
// what it routed and the files it kept aside in conflicts.
const agentState = "/var/lib/hangar-agent"

// filesPort is the vsock port the worker serves them on.
const filesPort = 2049

// sharedOptions are the NFS mount's. Only names that are there are cached,
// and what is known of a file or directory for a second: a file another
// environment makes is seen at once, and one it removes -- a lock directory
// let go of -- within a second.
var sharedOptions = fmt.Sprintf("vers=4.2,proto=vsock,addr=vsock:%d,port=%d,hard,lookupcache=positive,actimeo=1",
	vsock.CIDHost, filesPort)

// hangarfs's routes (include/uapi/linux/hangarfs.h).
type hangarfsRoutes struct {
	SharedFd int32
	Len      uint32
	Buf      uint64
}

const (
	hangarfsRouteFile    = 'f'
	hangarfsRouteDir     = 'd'
	hangarfsRouteExclude = 'x'
	// _IOW('H', 1, struct hangarfs_routes)
	hangarfsIocRoutes = 1<<30 | uint(unsafe.Sizeof(hangarfsRoutes{}))<<16 | 'H'<<8 | 1
)

// router routes the shared files' paths, for the user, to the worker's.
type router struct {
	uid, gid int

	mu      sync.Mutex
	mounted bool
}

func newRouter(u *user.User) *router {
	r := &router{}
	r.uid, _ = strconv.Atoi(u.Uid)
	r.gid, _ = strconv.Atoi(u.Gid)
	return r
}

func (r *router) mount() error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.mounted {
		return nil
	}
	if err := os.MkdirAll(sharedDir, 0o755); err != nil {
		return err
	}
	if err := unix.Mount("hangar:/", sharedDir, "nfs4", unix.MS_NOSUID|unix.MS_NODEV, sharedOptions); err != nil {
		return fmt.Errorf("mounting the shared files: %w", err)
	}
	r.mounted = true
	return nil
}

// apply has hangarfs serve each route's path from its target, making the
// directories a target is in -- and a directory's target -- if they are
// not there: a name whose target's directory is missing cannot be made.
func (r *router) apply(routes []profile.Route) error {
	if err := r.mount(); err != nil {
		return err
	}
	var buf []byte
	for _, rt := range routes {
		dir := path.Dir(rt.Target)
		kind := byte(hangarfsRouteFile)
		switch {
		case rt.Exclude:
			kind = hangarfsRouteExclude
		case rt.Dir:
			dir, kind = rt.Target, hangarfsRouteDir
		}
		if !rt.Exclude {
			if err := r.mkdirAll(filepath.Join(sharedDir, dir)); err != nil {
				return fmt.Errorf("making %s in the shared files: %w", dir, err)
			}
		}
		buf = append(buf, kind)
		buf = append(buf, rt.Path...)
		buf = append(buf, 0)
		buf = append(buf, rt.Target...)
		buf = append(buf, 0)
	}
	shared, err := os.Open(sharedDir)
	if err != nil {
		return err
	}
	defer shared.Close()
	root, err := os.Open("/")
	if err != nil {
		return err
	}
	defer root.Close()
	arg := hangarfsRoutes{SharedFd: int32(shared.Fd()), Len: uint32(len(buf))}
	if len(buf) > 0 {
		arg.Buf = uint64(uintptr(unsafe.Pointer(&buf[0])))
	}
	if _, _, e := unix.Syscall(unix.SYS_IOCTL, root.Fd(), uintptr(hangarfsIocRoutes), uintptr(unsafe.Pointer(&arg))); e != 0 {
		return fmt.Errorf("setting hangarfs's routes: %w", e)
	}
	return nil
}

// mkdirAll makes a directory and any parents missing, owned by the user.
func (r *router) mkdirAll(dir string) error {
	if _, err := os.Stat(dir); err == nil {
		return nil
	}
	if parent := filepath.Dir(dir); parent != dir {
		if err := r.mkdirAll(parent); err != nil {
			return err
		}
	}
	if err := os.Mkdir(dir, 0o755); err != nil && !errors.Is(err, fs.ErrExist) {
		return err
	}
	return os.Lchown(dir, r.uid, r.gid)
}
