// Package packs keeps the files environments share.
//
// A pack is a named list of paths and holds no files. A copy is one set of
// a pack's files, a directory on the files root named by the copy's ID,
// which every worker serves its environments over NFS (internal/nfs) and
// each environment's agent routes the pack's paths to (hangarfs). A copy
// is a person's own, made when they first need it, or shared: a team's,
// or one a person keeps apart from their own.
//
// An environment has the packs its template lists, then those that attach
// themselves to it -- its owner's, their teams', everyone's -- unless its
// template turns those off. For each it uses one copy: the one chosen on
// the environment, else the template's pin, else the one it was first
// given, else its owner's default, else their own. The profile is a pack
// like any other: built in, attached to everyone, and changed by admins.
package packs

import (
	"fmt"
	"path"
	"slices"
	"strings"
)

// Home starts a path in the home directory. A copy keeps such a file
// under ~/ in its directory, and an absolute one at its path.
const Home = "~/"

// Exclude starts a path left out of a shared directory: it stays each
// environment's own.
const Exclude = "!"

// MaxPaths is the most paths one pack names.
const MaxPaths = 64

// MaxFileSize is the largest file a copy holds through the API.
const MaxFileSize = 4 << 20

// MaxCopySize is the most a copy holds in all, through the API.
const MaxCopySize = 64 << 20

// Path is one of a pack's paths.
type Path struct {
	// Path is ~/ in the home directory or absolute; a directory's ends in
	// a slash, and one left out starts with Exclude.
	Path string
	// Sensitive keeps every file under the path, in every copy, from
	// environments given no sensitive files.
	Sensitive bool
}

// ValidKey reports whether p names a file of a copy: a clean path in the
// home directory, ~/ and more, or a clean absolute one.
func ValidKey(p string) bool {
	if strings.ContainsRune(p, 0) {
		return false
	}
	if rel, ok := strings.CutPrefix(p, Home); ok {
		return rel != "" && rel != "." && rel != ".." && path.Clean(rel) == rel &&
			!strings.HasPrefix(rel, "/") && !strings.HasPrefix(rel, "../")
	}
	return strings.HasPrefix(p, "/") && p != "/" && path.Clean(p) == p &&
		p != "/~" && !strings.HasPrefix(p, "/~/")
}

// refusedAbsolute are the kernel's and the system's own, where no pack
// puts files.
var refusedAbsolute = []string{"/proc/", "/sys/", "/dev/", "/run/", "/boot/"}

// CheckPath reports why a path cannot be one of a pack's, or nil.
func CheckPath(p string) error {
	x := strings.TrimPrefix(p, Exclude)
	clean := strings.TrimSuffix(x, "/")
	if !ValidKey(clean) || strings.HasSuffix(x, "//") {
		return fmt.Errorf("%q is neither a clean path in the home directory (~/…) nor a clean absolute one", x)
	}
	for _, r := range refusedAbsolute {
		if strings.HasPrefix(clean+"/", r) {
			return fmt.Errorf("%s is the system's, and holds no pack's files", r)
		}
	}
	return nil
}

// Paths are a pack's paths, as Path.Path has them.
type Paths []string

// PathsOf are the paths of ps.
func PathsOf(ps []Path) Paths {
	out := make(Paths, 0, len(ps))
	for _, p := range ps {
		out = append(out, p.Path)
	}
	return out
}

// under reports whether p is s, or under s, a directory.
func under(p, s string) bool {
	return p == strings.TrimSuffix(s, "/") || p == s || strings.HasSuffix(s, "/") && strings.HasPrefix(p, s)
}

// Shared reports whether a file is shared: at one of the paths, or under
// one, and not left out.
func (ps Paths) Shared(p string) bool {
	if !ValidKey(p) {
		return false
	}
	shared := false
	for _, s := range ps {
		if x, ok := strings.CutPrefix(s, Exclude); ok {
			if under(p, x) {
				return false
			}
			continue
		}
		if under(p, s) {
			shared = true
		}
	}
	return shared
}

// Routed are the paths an environment routes to a copy: those no
// directory among them already shares, and the exclusions inside a shared
// directory.
func (ps Paths) Routed() Paths {
	var out Paths
	for _, p := range ps {
		x, exclude := strings.CutPrefix(p, Exclude)
		covered := slices.ContainsFunc(ps, func(s string) bool {
			return s != p && !strings.HasPrefix(s, Exclude) && strings.HasSuffix(s, "/") &&
				strings.HasPrefix(x, s)
		})
		if exclude == covered && !slices.Contains(out, p) {
			out = append(out, p)
		}
	}
	return out
}

// Sensitive reports whether a file is under a sensitive path of ps.
func Sensitive(ps []Path, p string) bool {
	for _, s := range ps {
		if s.Sensitive && !strings.HasPrefix(s.Path, Exclude) && under(p, s.Path) {
			return true
		}
	}
	return false
}

// checkPaths checks a pack's paths, returning them as kept: trimmed,
// without duplicates, in order. A path left out must be inside a directory
// the pack shares.
func checkPaths(in []Path) ([]Path, error) {
	if len(in) > MaxPaths {
		return nil, fmt.Errorf("%w: a pack names at most %d paths", ErrInvalid, MaxPaths)
	}
	var out []Path
	for _, p := range in {
		p.Path = strings.TrimSpace(p.Path)
		if err := CheckPath(p.Path); err != nil {
			return nil, fmt.Errorf("%w: %v", ErrInvalid, err)
		}
		if i := slices.IndexFunc(out, func(q Path) bool { return q.Path == p.Path }); i >= 0 {
			out[i].Sensitive = out[i].Sensitive || p.Sensitive
			continue
		}
		if strings.HasPrefix(p.Path, Exclude) {
			p.Sensitive = false
		}
		out = append(out, p)
	}
	all := PathsOf(out)
	for _, p := range all {
		x, ok := strings.CutPrefix(p, Exclude)
		if !ok {
			continue
		}
		if !slices.ContainsFunc(all, func(s string) bool {
			return !strings.HasPrefix(s, Exclude) && strings.HasSuffix(s, "/") && strings.HasPrefix(x, s) && x != s
		}) {
			return nil, fmt.Errorf("%w: %s is left out of no directory the pack shares", ErrInvalid, x)
		}
	}
	slices.SortStableFunc(out, func(a, b Path) int {
		return strings.Compare(strings.TrimPrefix(a.Path, Exclude), strings.TrimPrefix(b.Path, Exclude))
	})
	return out, nil
}
