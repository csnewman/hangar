package packs

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path"
	"sort"
	"strings"
	"time"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/users"
)

// File is one of a copy's files.
type File struct {
	// Path is the file's key: ~/ in the home directory, or absolute.
	Path string
	Data []byte // only when read on its own
	Size int64
	Mode uint32
	// Sensitive is under a sensitive path of the pack.
	Sensitive bool
	// Shared is under a path the pack shares. A copy keeps a file the pack
	// stopped sharing until someone deletes it.
	Shared    bool
	UpdatedAt time.Time
}

// stagingPrefix names files written beside one they replace: this store's,
// and hangarfs's for a save renamed over a routed name. They are not a
// copy's files.
const stagingPrefix = ".hangarfs-"

// keyPath is where a copy's file is under the root: a key in the home
// directory under ~/ in the copy's directory, an absolute one at its path.
func keyPath(copy, key string) string { return copy + "/" + strings.TrimPrefix(key, "/") }

// keyOf is the key of the file at p, a path under the root, in copy.
func keyOf(copy, p string) string {
	rel := strings.TrimPrefix(p, copy+"/")
	if strings.HasPrefix(rel, Home) {
		return rel
	}
	return "/" + rel
}

// copyAndPack returns a copy p may use, and its pack's paths.
func (s *Store) copyAndPack(ctx context.Context, p users.Principal, id string) (Copy, []Path, error) {
	if !db.ValidUUID(id) {
		return Copy{}, nil, ErrNotFound
	}
	var c Copy
	var k Pack
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		var err error
		if c, err = getCopy(ctx, tx, p, id); err != nil {
			return err
		}
		k, err = anyPack(ctx, tx, c.PackID)
		return err
	})
	return c, k.Paths, err
}

// Files returns a copy's files, without their contents.
func (s *Store) Files(ctx context.Context, p users.Principal, copy string) ([]File, error) {
	c, paths, err := s.copyAndPack(ctx, p, copy)
	if err != nil {
		return nil, err
	}
	return s.files(c.ID, paths)
}

func (s *Store) files(copy string, paths []Path) ([]File, error) {
	shared := PathsOf(paths)
	out := []File{}
	err := fs.WalkDir(s.root.FS(), copy, func(p string, e fs.DirEntry, err error) error {
		if err != nil {
			if errors.Is(err, fs.ErrNotExist) {
				return fs.SkipAll
			}
			return err
		}
		if !e.Type().IsRegular() || strings.HasPrefix(e.Name(), stagingPrefix) {
			return nil
		}
		info, err := e.Info()
		if err != nil {
			return nil
		}
		key := keyOf(copy, p)
		out = append(out, File{Path: key, Size: info.Size(), Mode: uint32(info.Mode().Perm()),
			Sensitive: Sensitive(paths, key), Shared: shared.Shared(key), UpdatedAt: info.ModTime()})
		return nil
	})
	sort.Slice(out, func(i, j int) bool { return out[i].Path < out[j].Path })
	return out, err
}

// File returns one of a copy's files with its contents, or ErrNotFound.
func (s *Store) File(ctx context.Context, p users.Principal, copy, key string) (File, error) {
	if !ValidKey(key) {
		return File{}, ErrNotFound
	}
	c, paths, err := s.copyAndPack(ctx, p, copy)
	if err != nil {
		return File{}, err
	}
	return s.file(c.ID, paths, key)
}

func (s *Store) file(copy string, paths []Path, key string) (File, error) {
	info, err := s.root.Lstat(keyPath(copy, key))
	if errors.Is(err, fs.ErrNotExist) || err == nil && !info.Mode().IsRegular() {
		return File{}, ErrNotFound
	}
	if err != nil {
		return File{}, err
	}
	data, err := s.root.ReadFile(keyPath(copy, key))
	if err != nil {
		return File{}, err
	}
	return File{Path: key, Data: data, Size: int64(len(data)), Mode: uint32(info.Mode().Perm()),
		Sensitive: Sensitive(paths, key), Shared: PathsOf(paths).Shared(key), UpdatedAt: info.ModTime()}, nil
}

// size is what a copy's files take, but the one at except.
func (s *Store) size(copy, except string) int64 {
	var total int64
	fs.WalkDir(s.root.FS(), copy, func(p string, e fs.DirEntry, err error) error {
		if err == nil && e.Type().IsRegular() && p != except {
			if info, err := e.Info(); err == nil {
				total += info.Size()
			}
		}
		return nil
	})
	return total
}

// writable returns a copy p may change the files of, and its pack's paths.
func (s *Store) writable(ctx context.Context, p users.Principal, copy string) (Copy, []Path, error) {
	c, paths, err := s.copyAndPack(ctx, p, copy)
	if err == nil && !c.CanWrite {
		err = fmt.Errorf("%w: only the copy's owner, or its team's members, change its files", ErrForbidden)
	}
	return c, paths, err
}

// Put writes a file at a path the pack shares, returning it as stored. It
// is written beside its name and renamed over it, so an environment
// reading it sees it whole. A mode of 0 is 0600 for a sensitive file and
// 0644 for another, or the file's own if it has one.
func (s *Store) Put(ctx context.Context, p users.Principal, copy, key string, data []byte, mode uint32) (File, error) {
	c, paths, err := s.writable(ctx, p, copy)
	if err != nil {
		return File{}, err
	}
	if !PathsOf(paths).Shared(key) {
		return File{}, fmt.Errorf("%w: the pack does not share %s", ErrInvalid, key)
	}
	if len(data) > MaxFileSize {
		return File{}, fmt.Errorf("%w: %s is larger than %d bytes", ErrInvalid, key, MaxFileSize)
	}
	target := keyPath(c.ID, key)
	if s.size(c.ID, target)+int64(len(data)) > MaxCopySize {
		return File{}, fmt.Errorf("%w: the copy would hold more than %d MiB", ErrInvalid, MaxCopySize>>20)
	}
	mode &= 0o777
	if mode == 0 {
		if info, err := s.root.Lstat(target); err == nil {
			mode = uint32(info.Mode().Perm())
		} else if Sensitive(paths, key) {
			mode = 0o600
		} else {
			mode = 0o644
		}
	}
	if err := s.root.MkdirAll(path.Dir(target), 0o755); err != nil {
		return File{}, err
	}
	if err := s.replace(target, data, os.FileMode(mode)); err != nil {
		return File{}, err
	}
	if err := s.record(ctx, "pack_copy.file_write", c, key, map[string]any{"size": len(data)}); err != nil {
		return File{}, err
	}
	return s.file(c.ID, paths, key)
}

// Chmod changes a file's mode.
func (s *Store) Chmod(ctx context.Context, p users.Principal, copy, key string, mode uint32) (File, error) {
	if !ValidKey(key) {
		return File{}, ErrNotFound
	}
	mode &= 0o777
	if mode == 0 {
		return File{}, fmt.Errorf("%w: a file needs a mode", ErrInvalid)
	}
	c, paths, err := s.writable(ctx, p, copy)
	if err != nil {
		return File{}, err
	}
	info, err := s.root.Lstat(keyPath(c.ID, key))
	if errors.Is(err, fs.ErrNotExist) || err == nil && !info.Mode().IsRegular() {
		return File{}, ErrNotFound
	}
	if err != nil {
		return File{}, err
	}
	if err := s.root.Chmod(keyPath(c.ID, key), os.FileMode(mode)); err != nil {
		return File{}, err
	}
	if err := s.record(ctx, "pack_copy.file_mode", c, key, map[string]any{"mode": fmt.Sprintf("%#o", mode)}); err != nil {
		return File{}, err
	}
	return s.file(c.ID, paths, key)
}

// Delete removes a file from a copy, and from every environment using it.
func (s *Store) Delete(ctx context.Context, p users.Principal, copy, key string) error {
	if !ValidKey(key) {
		return fmt.Errorf("%w: %q is not a file's path", ErrInvalid, key)
	}
	c, _, err := s.writable(ctx, p, copy)
	if err != nil {
		return err
	}
	if err := s.root.Remove(keyPath(c.ID, key)); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	return s.record(ctx, "pack_copy.file_delete", c, key, nil)
}

func (s *Store) record(ctx context.Context, action string, c Copy, key string, details map[string]any) error {
	return s.db.Transact(ctx, func(tx db.Tx) error {
		return audit.Record(ctx, tx, audit.Event{Action: action,
			Target:  audit.Ref{Type: audit.KindFile, ID: c.ID + ":" + key, Name: key},
			Related: []audit.Ref{copyRef(c)}, Details: details})
	})
}

// replace writes data beside name and renames it over name.
func (s *Store) replace(name string, data []byte, mode os.FileMode) error {
	tmp := path.Join(path.Dir(name), stagingPrefix+randomName())
	f, err := s.root.OpenFile(tmp, os.O_WRONLY|os.O_CREATE|os.O_EXCL, mode)
	if err != nil {
		return err
	}
	_, err = f.Write(data)
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	if err == nil {
		// Set apart from the umask the file was made under.
		err = s.root.Chmod(tmp, mode)
	}
	if err == nil {
		err = s.root.Rename(tmp, name)
	}
	if err != nil {
		s.root.Remove(tmp)
	}
	return err
}

func randomName() string {
	b := make([]byte, 6)
	rand.Read(b)
	return hex.EncodeToString(b)
}
