package templates

import (
	"fmt"
	"maps"
	"path"
	"regexp"
	"strings"

	"github.com/csnewman/hangar/internal/api"
)

// Limits on what a template may ask for.
const (
	MaxCPUs      = 64
	MinMemoryMiB = 512
	MaxMemoryMiB = 256 * 1024
	MaxRepos     = 16
)

// An environment's name becomes its hostname and part of URLs, so whatever
// else a template demands of it, it is always a DNS label.
var envName = regexp.MustCompile(`^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$`)

// Label keys and values follow Kubernetes' rules, so a selector can be
// written the same way whichever backend the workers run on.
var (
	labelKey   = regexp.MustCompile(`^([a-z0-9]([-a-z0-9.]*[a-z0-9])?/)?[A-Za-z0-9]([-A-Za-z0-9_.]{0,61}[A-Za-z0-9])?$`)
	labelValue = regexp.MustCompile(`^([A-Za-z0-9]([-A-Za-z0-9_.]{0,61}[A-Za-z0-9])?)?$`)
)

// Validate checks a template's spec and returns it with defaults filled in.
func Validate(s api.TemplateSpec) (api.TemplateSpec, error) {
	bad := func(format string, a ...any) (api.TemplateSpec, error) {
		return s, fmt.Errorf("%w: "+format, append([]any{ErrInvalid}, a...)...)
	}

	s.Image = strings.TrimSpace(s.Image)
	if s.Image == "" {
		return bad("an image is required")
	}
	if s.CPUs < 1 || s.CPUs > MaxCPUs {
		return bad("vCPUs must be between 1 and %d", MaxCPUs)
	}
	if s.MemoryMiB < MinMemoryMiB || s.MemoryMiB > MaxMemoryMiB {
		return bad("memory must be between %d and %d MiB", MinMemoryMiB, MaxMemoryMiB)
	}

	switch s.Display {
	case api.DisplayNone, api.DisplayDesktop:
	case "":
		s.Display = api.DisplayDesktop
	default:
		return bad("display must be none or desktop")
	}
	switch s.GPU {
	case api.GPUNone, api.GPUVirtual, api.GPUPassthrough:
	case "":
		s.GPU = api.GPUNone
	default:
		return bad("gpu must be none, virtual or passthrough")
	}

	if len(s.Repos) > MaxRepos {
		return bad("at most %d repositories", MaxRepos)
	}
	if s.Repos == nil {
		s.Repos = []api.Repo{}
	}
	paths := map[string]bool{}
	for i := range s.Repos {
		r := &s.Repos[i]
		r.URL, r.Ref, r.Path, r.Branch = strings.TrimSpace(r.URL), strings.TrimSpace(r.Ref),
			strings.TrimSpace(r.Path), strings.TrimSpace(r.Branch)
		if !gitURL(r.URL) {
			return bad("repository %d: %q is not a git URL (https://, ssh://, git@host:path or git://)", i+1, r.URL)
		}
		if !absPath(r.Path) {
			return bad("repository %d: the path must be absolute", i+1)
		}
		r.Path = path.Clean(r.Path)
		if paths[r.Path] {
			return bad("two repositories are cloned to %s", r.Path)
		}
		paths[r.Path] = true
		if r.Ref != "" && !refName(r.Ref) {
			return bad("repository %d: %q is not a valid ref", i+1, r.Ref)
		}
		// The branch is checked with a stand-in name: the real one is only
		// known when an environment is made, and is checked again then.
		if r.Branch != "" && !refName(strings.ReplaceAll(r.Branch, "{name}", "name")) {
			return bad("repository %d: %q is not a valid branch name", i+1, r.Branch)
		}
	}

	s.EditorPath = strings.TrimSpace(s.EditorPath)
	if s.EditorPath != "" {
		if !absPath(s.EditorPath) {
			return bad("the editor folder must be an absolute path")
		}
		s.EditorPath = path.Clean(s.EditorPath)
	}

	for i, f := range s.TrustedFolders {
		f = strings.TrimSpace(f)
		if !absPath(f) {
			return bad("trusted folder %d must be an absolute path", i+1)
		}
		s.TrustedFolders[i] = path.Clean(f)
	}

	s.NamePattern = strings.TrimSpace(s.NamePattern)
	s.NameHint = strings.TrimSpace(s.NameHint)
	if s.NamePattern != "" {
		if _, err := namePattern(s.NamePattern); err != nil {
			return bad("the name pattern is not a valid regular expression: %v", err)
		}
	}

	if s.Placement == nil {
		s.Placement = map[string]string{}
	}
	for k, v := range s.Placement {
		if !labelKey.MatchString(k) || len(k) > 253 {
			return bad("placement: %q is not a valid label key", k)
		}
		if !labelValue.MatchString(v) {
			return bad("placement: %q is not a valid label value", v)
		}
	}
	return s, nil
}

// namePattern compiles a template's name rule so it must match the whole
// name. Authors write "PROJ-[0-9]+", not "^PROJ-[0-9]+$", and a pattern that
// matched only part of a name would be a surprise.
func namePattern(p string) (*regexp.Regexp, error) {
	return regexp.Compile(`^(?:` + p + `)$`)
}

// Resolve turns a template's spec into an environment's: it checks the name
// against the template's rule, and fills the name into branch patterns.
func Resolve(t api.TemplateSpec, name string) (api.Spec, error) {
	if !envName.MatchString(name) {
		return api.Spec{}, fmt.Errorf("%w: a name is lowercase letters, digits and hyphens, at most 63 characters", ErrInvalid)
	}
	if t.NamePattern != "" {
		re, err := namePattern(t.NamePattern)
		if err != nil {
			return api.Spec{}, err
		}
		if !re.MatchString(name) {
			hint := t.NameHint
			if hint == "" {
				hint = "it must match " + t.NamePattern
			}
			return api.Spec{}, fmt.Errorf("%w: this template does not accept the name %q: %s", ErrInvalid, name, hint)
		}
	}

	s := t.Spec
	s.Repos = make([]api.Repo, len(t.Repos))
	for i, r := range t.Repos {
		r.Branch = strings.ReplaceAll(r.Branch, "{name}", name)
		if r.Branch != "" && !refName(r.Branch) {
			return api.Spec{}, fmt.Errorf("%w: the name %q makes an invalid branch name %q", ErrInvalid, name, r.Branch)
		}
		s.Repos[i] = r
	}
	if s.EditorPath == "" && len(s.Repos) > 0 {
		// The editor opens on what the environment is for.
		s.EditorPath = s.Repos[0].Path
	}
	s.Placement = maps.Clone(t.Placement)
	if s.Placement == nil {
		s.Placement = map[string]string{}
	}
	return s, nil
}

func absPath(p string) bool {
	return strings.HasPrefix(p, "/") && !strings.ContainsAny(p, "\x00\n")
}

var scpLike = regexp.MustCompile(`^[A-Za-z0-9._-]+@[A-Za-z0-9.-]+:[^\s]+$`)

func gitURL(u string) bool {
	if strings.ContainsAny(u, " \t\n\x00") {
		return false
	}
	for _, scheme := range []string{"https://", "http://", "ssh://", "git://"} {
		if rest, ok := strings.CutPrefix(u, scheme); ok {
			return len(rest) > 0 && !strings.HasPrefix(rest, "/")
		}
	}
	return scpLike.MatchString(u)
}

// refName applies git's rules for a ref name (git check-ref-format), so a
// template cannot name a branch git would refuse to create.
func refName(r string) bool {
	if r == "" || r == "@" || len(r) > 255 {
		return false
	}
	if strings.HasPrefix(r, "/") || strings.HasSuffix(r, "/") || strings.HasSuffix(r, ".") ||
		strings.HasSuffix(r, ".lock") || strings.HasPrefix(r, "-") {
		return false
	}
	for _, bad := range []string{"..", "//", "@{", "/."} {
		if strings.Contains(r, bad) {
			return false
		}
	}
	for _, c := range r {
		if c < 0x20 || c == 0x7f || strings.ContainsRune(" ~^:?*[\\", c) {
			return false
		}
	}
	return !strings.HasPrefix(r, ".")
}
