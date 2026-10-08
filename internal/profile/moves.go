package profile

import (
	"bytes"
	"encoding/json"
	"errors"
	"io/fs"
	"os"
	"path"
	"path/filepath"
	"strings"
)

// GuestFiles is how the guest reaches the shared files.
type GuestFiles struct {
	// Mount makes Shared the copies' files, if it is not yet.
	Mount func() error
	// Route has the routes applied: each route's path served from its
	// target under Shared.
	Route func([]Route) error
	// Shared is where the copies are: <Shared>/<copy>/<path in copy>.
	Shared string
	// State is a directory on the environment's own disk, kept across
	// restarts: the routes last applied, and the files kept aside in
	// conflicts.
	State string
}

// served is where a guest path is served from under routes: the target,
// a copy's file under Shared, or nothing, for the guest's own disk. As
// hangarfs does, an exclusion wins, then a route at the path itself, then
// the nearest routed directory it is in.
func served(routes []Route, p string) string {
	best := -1
	for i, r := range routes {
		if r.Exclude {
			if p == r.Path || strings.HasPrefix(p, r.Path+"/") {
				return ""
			}
			continue
		}
		if p == r.Path {
			return r.Target
		}
		if r.Dir && strings.HasPrefix(p, r.Path+"/") && (best < 0 || len(r.Path) > len(routes[best].Path)) {
			best = i
		}
	}
	if best < 0 {
		return ""
	}
	return routes[best].Target + strings.TrimPrefix(p, routes[best].Path)
}

// copyOf is the copy a target is in, and the file's key in it: ~/ in the
// home directory, or absolute.
func copyOf(target string) (copy, key string) {
	copy, rel, _ := strings.Cut(target, "/")
	if strings.HasPrefix(rel, "~/") {
		return copy, rel
	}
	return copy, "/" + rel
}

// down is a file to be copied down once the routes are applied: the path
// stops being shared, and the environment keeps the copy's file as its
// own.
type down struct {
	path string
	data []byte
	mode fs.FileMode
}

// conflict is a path the environment had a file of its own at, differing
// from the copy's, as the path became shared.
type conflict struct{ copy, key string }

// sortPaths sorts, before routes are applied in place of old, each file
// whose sharing changes. A file the environment has, at a path becoming
// shared, is copied up into the copy, or kept aside as a conflict if the
// copy has a different one. A copy's file at a path no longer shared is
// read, for down to write once the routes are applied. A path that moves
// from one copy to another is neither: the environment simply sees the
// other copy's.
func (s *guestSession) sortPaths(old, routes []Route) ([]down, []conflict) {
	files := s.g.files
	var downs []down
	var conflicts []conflict
	seen := map[string]bool{}

	// Becoming shared: walk the environment's own files under every path
	// the new routes share, or old ones left out.
	var roots []string
	for _, r := range routes {
		if !r.Exclude {
			roots = append(roots, r.Path)
		}
	}
	for _, r := range old {
		if r.Exclude {
			roots = append(roots, r.Path)
		}
	}
	for _, root := range roots {
		filepath.WalkDir(root, func(p string, e fs.DirEntry, err error) error {
			if err != nil || !e.Type().IsRegular() || seen[p] {
				return nil
			}
			seen[p] = true
			was, now := served(old, p), served(routes, p)
			if was != "" || now == "" {
				return nil
			}
			local, err := os.ReadFile(p)
			if err != nil {
				return nil
			}
			info, err := e.Info()
			if err != nil {
				return nil
			}
			shared := filepath.Join(files.Shared, now)
			theirs, err := os.ReadFile(shared)
			switch {
			case errors.Is(err, fs.ErrNotExist):
				if err := writeFile(shared, local, info.Mode().Perm(), -1, -1); err != nil {
					s.g.log.Debug("profile: copying a file up", "path", p, "err", err)
				}
			case err != nil:
			case !bytes.Equal(local, theirs):
				copy, key := copyOf(now)
				if err := writeFile(s.g.keptPath(copy, key), local, info.Mode().Perm(), -1, -1); err != nil {
					s.g.log.Warn("profile: keeping a file aside", "path", p, "err", err)
					return nil
				}
				conflicts = append(conflicts, conflict{copy: copy, key: key})
			}
			return nil
		})
	}

	// No longer shared: walk each old route's copy files, for those the
	// new routes leave to the environment.
	for _, r := range old {
		if r.Exclude {
			continue
		}
		base := filepath.Join(files.Shared, r.Target)
		filepath.WalkDir(base, func(p string, e fs.DirEntry, err error) error {
			if err != nil || !e.Type().IsRegular() {
				return nil
			}
			guest := r.Path + strings.TrimPrefix(p, base)
			if served(old, guest) != r.Target+strings.TrimPrefix(p, base) || served(routes, guest) != "" {
				return nil
			}
			data, err := os.ReadFile(p)
			if err != nil {
				return nil
			}
			info, err := e.Info()
			if err != nil {
				return nil
			}
			downs = append(downs, down{path: guest, data: data, mode: info.Mode().Perm()})
			return nil
		})
	}
	return downs, conflicts
}

// copyDown writes the files no longer shared into the environment, once
// the routes are applied, as the user's.
func (s *guestSession) copyDown(downs []down) {
	for _, d := range downs {
		if err := writeFile(d.path, d.data, d.mode, s.uid, s.gid); err != nil {
			s.g.log.Warn("profile: copying a file down", "path", d.path, "err", err)
		}
	}
}

// writeFile writes data at name, beside it and renamed over it, making
// the directories it is in; owned by uid and gid, when they are not -1.
func writeFile(name string, data []byte, mode fs.FileMode, uid, gid int) error {
	dir := filepath.Dir(name)
	if err := mkdirAllOwned(dir, uid, gid); err != nil {
		return err
	}
	f, err := os.CreateTemp(dir, ".hangarfs-")
	if err != nil {
		return err
	}
	tmp := f.Name()
	_, err = f.Write(data)
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	if err == nil {
		err = os.Chmod(tmp, mode)
	}
	if err == nil && uid >= 0 {
		err = os.Lchown(tmp, uid, gid)
	}
	if err == nil {
		err = os.Rename(tmp, name)
	}
	if err != nil {
		os.Remove(tmp)
	}
	return err
}

// mkdirAllOwned makes a directory and any parents missing, owned by uid
// and gid when they are not -1.
func mkdirAllOwned(dir string, uid, gid int) error {
	if _, err := os.Stat(dir); err == nil {
		return nil
	}
	if parent := filepath.Dir(dir); parent != dir {
		if err := mkdirAllOwned(parent, uid, gid); err != nil {
			return err
		}
	}
	if err := os.Mkdir(dir, 0o755); err != nil && !errors.Is(err, fs.ErrExist) {
		return err
	}
	if uid >= 0 {
		return os.Lchown(dir, uid, gid)
	}
	return nil
}

// keptPath is where a conflict's file the environment had is kept aside.
func (g *Guest) keptPath(copy, key string) string {
	return filepath.Join(g.files.State, "kept", copy, strings.TrimPrefix(key, "/"))
}

// routesFile is where the routes last applied are kept.
func (g *Guest) routesFile() string { return filepath.Join(g.files.State, "routes.json") }

// lastRoutes are the routes last applied, in this boot or before it.
func (g *Guest) lastRoutes() []Route {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.routes != nil {
		return g.routes
	}
	var routes []Route
	if b, err := os.ReadFile(g.routesFile()); err == nil {
		json.Unmarshal(b, &routes)
	}
	return routes
}

// keepRoutes records the routes applied.
func (g *Guest) keepRoutes(routes []Route) {
	b, err := json.Marshal(routes)
	if err == nil {
		err = writeFile(g.routesFile(), b, 0o600, -1, -1)
	}
	if err != nil {
		g.log.Warn("profile: keeping the routes applied", "err", err)
	}
}

// resolve does what the server says of a conflict: drops the file kept
// aside, or writes it over the copy's.
func (s *guestSession) resolve(m Message) error {
	kept := s.g.keptPath(m.Copy, m.Path)
	if m.Resolution == ResolveEnvironment {
		data, err := os.ReadFile(kept)
		if err == nil {
			info, _ := os.Stat(kept)
			rel := strings.TrimPrefix(m.Path, "/")
			err = writeFile(filepath.Join(s.g.files.Shared, m.Copy, rel), data, info.Mode().Perm(), -1, -1)
		}
		if err != nil && !errors.Is(err, fs.ErrNotExist) {
			return err
		}
	}
	if err := os.Remove(kept); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	// The directories the file was in, once nothing else is.
	for dir := path.Dir(kept); strings.HasPrefix(dir, filepath.Join(s.g.files.State, "kept")+"/"); dir = path.Dir(dir) {
		if os.Remove(dir) != nil {
			break
		}
	}
	return nil
}
