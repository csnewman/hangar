package profile

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/user"
	"path"
	"path/filepath"
	"strconv"
	"syscall"
	"time"

	"github.com/hanwen/go-fuse/v2/fs"
	"github.com/hanwen/go-fuse/v2/fuse"
)

// Gate decides whether a lock may be taken, and lets it go.
type Gate interface {
	// Lock reports whether the lock at path, relative to the home
	// directory, is this environment's to hold.
	Lock(path string) (bool, error)
	Unlock(path string) error
}

// LockFS serves each of LockDirs as a FUSE file system passing through to
// a directory of the same contents elsewhere, the backing. Everything is as
// it would be on the directory itself, except that creating one of the
// Locks asks gate first, and removing one tells it after.
//
// While a program creates or removes an entry, the kernel holds the
// directory it is in, so nothing the gate does then may go through the
// mount: it works on the backing.
type LockFS struct {
	// Backing maps each served directory, relative to the home directory,
	// to its backing.
	Backing map[string]string
	servers []*fuse.Server
}

// MountLockFS mounts LockFS over u's home directory. What is already in a
// directory it serves is moved into the backing first, and stays there
// for the next mount.
func MountLockFS(u *user.User, backingRoot string, gate Gate) (*LockFS, error) {
	uid, _ := strconv.Atoi(u.Uid)
	gid, _ := strconv.Atoi(u.Gid)
	l := &LockFS{Backing: map[string]string{}}
	for _, d := range LockDirs() {
		target := filepath.Join(u.HomeDir, d)
		backing := filepath.Join(backingRoot, d)
		if err := mkdirOwned(backing, uid, gid); err != nil {
			return nil, err
		}
		if err := mkdirOwned(target, uid, gid); err != nil {
			return nil, err
		}
		if err := moveContents(target, backing); err != nil {
			return nil, fmt.Errorf("moving %s aside: %w", target, err)
		}
		root := &fs.LoopbackRoot{Path: backing}
		st := syscall.Stat_t{}
		if err := syscall.Stat(backing, &st); err != nil {
			return nil, err
		}
		root.Dev = uint64(st.Dev)
		dir := d
		root.NewNode = func(r *fs.LoopbackRoot, parent *fs.Inode, name string, st *syscall.Stat_t) fs.InodeEmbedder {
			return &lockNode{LoopbackNode: fs.LoopbackNode{RootData: r}, dir: dir, gate: gate}
		}
		rootNode := root.NewNode(root, nil, "", &st)
		root.RootNode = rootNode
		// No entry or attribute is cached: the gate writes the backing
		// directly, and what it wrote must be what the next read sees.
		zero := time.Duration(0)
		server, err := fs.Mount(target, rootNode, &fs.Options{
			EntryTimeout:    &zero,
			AttrTimeout:     &zero,
			NegativeTimeout: &zero,
			MountOptions: fuse.MountOptions{
				AllowOther:  true,
				DirectMount: true,
				FsName:      "hangar-profile",
				Name:        "hangar",
				Options:     []string{"default_permissions"},
			},
		})
		if err != nil {
			return nil, fmt.Errorf("mounting %s: %w", target, err)
		}
		l.servers = append(l.servers, server)
		l.Backing[d] = backing
	}
	return l, nil
}

// Unmount takes the mounts down.
func (l *LockFS) Unmount() {
	for _, s := range l.servers {
		s.Unmount()
	}
}

// lockNode is a passthrough node whose directory may have locks made in it.
type lockNode struct {
	fs.LoopbackNode
	dir  string // the served directory, relative to the home directory
	gate Gate
}

// lockPath is the path, relative to the home directory, of an entry name
// in this node, and whether it is one of the Locks.
func (n *lockNode) lockPath(name string) (string, bool) {
	p := path.Join(n.dir, n.Path(n.Root()), name)
	_, ok := LockAt(p)
	return p, ok
}

func (n *lockNode) Mkdir(ctx context.Context, name string, mode uint32, out *fuse.EntryOut) (*fs.Inode, syscall.Errno) {
	p, isLock := n.lockPath(name)
	if !isLock {
		return n.LoopbackNode.Mkdir(ctx, name, mode, out)
	}
	held, err := n.gate.Lock(p)
	if err != nil || !held {
		// Held elsewhere, or nobody to ask: to the program, it is taken.
		return nil, syscall.EEXIST
	}
	ch, errno := n.LoopbackNode.Mkdir(ctx, name, mode, out)
	if errno != 0 {
		n.gate.Unlock(p)
	}
	return ch, errno
}

func (n *lockNode) Create(ctx context.Context, name string, flags, mode uint32, out *fuse.EntryOut) (*fs.Inode, fs.FileHandle, uint32, syscall.Errno) {
	p, isLock := n.lockPath(name)
	if !isLock {
		return n.LoopbackNode.Create(ctx, name, flags, mode, out)
	}
	held, err := n.gate.Lock(p)
	if err != nil || !held {
		return nil, nil, 0, syscall.EEXIST
	}
	ch, fh, ff, errno := n.LoopbackNode.Create(ctx, name, flags, mode, out)
	if errno != 0 {
		n.gate.Unlock(p)
	}
	return ch, fh, ff, errno
}

func (n *lockNode) Rmdir(ctx context.Context, name string) syscall.Errno {
	p, isLock := n.lockPath(name)
	if isLock {
		n.gate.Unlock(p)
	}
	return n.LoopbackNode.Rmdir(ctx, name)
}

func (n *lockNode) Unlink(ctx context.Context, name string) syscall.Errno {
	p, isLock := n.lockPath(name)
	if isLock {
		n.gate.Unlock(p)
	}
	return n.LoopbackNode.Unlink(ctx, name)
}

func mkdirOwned(dir string, uid, gid int) error {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	return os.Lchown(dir, uid, gid)
}

// moveContents moves everything in from into to, unless it is already a
// mount: from an earlier run, the move is done.
func moveContents(from, to string) error {
	if mounted(from) {
		return errors.New("already mounted")
	}
	entries, err := os.ReadDir(from)
	if err != nil {
		return err
	}
	for _, e := range entries {
		dst := filepath.Join(to, e.Name())
		if _, err := os.Lstat(dst); err == nil {
			// The backing's copy is the one in use; this one is left
			// where it is, under the mount, rather than lost.
			continue
		}
		if err := os.Rename(filepath.Join(from, e.Name()), dst); err != nil {
			return err
		}
	}
	return nil
}

// mounted reports whether dir is a mount point: its device differs from
// its parent's.
func mounted(dir string) bool {
	var a, b syscall.Stat_t
	if syscall.Stat(dir, &a) != nil || syscall.Stat(filepath.Dir(dir), &b) != nil {
		return false
	}
	return a.Dev != b.Dev
}
