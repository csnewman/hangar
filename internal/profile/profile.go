// Package profile is the files environments share with others -- each
// user's profile, which follows them into every environment they own, and
// the packs templates give theirs -- and users' SSH keys.
//
// A profile is Claude's settings and sign-in, VS Code's settings, git's,
// and whatever else its owner chooses; a pack is files at fixed paths, a
// project's .env say. Each is a file set: a directory of the files root,
// which the control plane reads and writes for the web UI and every worker
// serves its environments over NFS (internal/nfs). An environment's agent
// mounts that, and hangarfs routes the sets' paths to it, so there is one
// copy of each file, which every environment given it reads and writes, and
// a lock taken on one in one environment holds in all of them -- whether by
// flock or fcntl, or, as Claude's sign-in refresh does, by making a
// directory.
//
// # Shared paths
//
// DefaultPaths are shared for everyone. A user adds more of their own -- a
// file, or a directory and everything under it -- and leaves paths inside a
// shared directory out, written with a leading "!"; UserPaths is the set a
// user has. What programs rewrite all the time or keep per machine
// (neverShared) is left out of any shared directory that holds it.
//
// # SSH keys
//
// Private keys never enter a guest. The agent serves an SSH agent socket,
// and the server signs with the key.
//
// # Hangar's registry
//
// The agent serves a Docker credential helper for Hangar's registry, and
// asks the server for a credential each time it is used: one good for an
// hour, as the environment's owner, which nothing stores. An environment
// not trusted with its owner's credentials is sent none.
//
// # Protocol
//
// A session is JSON lines both ways on one stream, which the server opens
// to the agent's Port. Each line is a Message.
package profile

import (
	"fmt"
	"path"
	"slices"
	"strings"
)

// Port is the vsock port the guest agent serves profile sessions on.
const Port uint32 = 8105

// Message types.
const (
	// Server to agent.
	TypePaths  = "paths"  // the file sets given, the profile's and packs', and their paths; again when they change
	TypeKeys   = "keys"   // the public keys the SSH agent offers
	TypeSigned = "signed" // the answer to a sign
	// Hangar's registry's host, sent first when there is one and the
	// environment is trusted with its owner's credentials.
	TypeRegistry   = "registry"
	TypeCredential = "credential" // the answer to a get_credential

	// Agent to server.
	TypeSign = "sign" // sign with one of the keys
	// A credential for Hangar's registry, as the environment's owner.
	TypeGetCredential = "get_credential"
)

// Message is one line of a session.
type Message struct {
	Type string `json:"type"`

	// Sets are the file sets the environment is given, the owner's
	// profile first, with the paths each shares.
	Sets []SetPaths `json:"sets,omitempty"`

	// Keys are authorized-keys lines.
	Keys []string `json:"keys,omitempty"`

	// A request and its answer share an ID. For a signature, Key is the
	// public key in SSH wire form, Data what to sign, and Signature an
	// ssh.Signature, marshalled.
	ID        int64  `json:"id,omitempty"`
	Data      []byte `json:"data,omitempty"`
	Key       []byte `json:"key,omitempty"`
	Flags     uint32 `json:"flags,omitempty"`
	Signature []byte `json:"signature,omitempty"`
	Error     string `json:"error,omitempty"`

	// Hangar's registry: its host, and a credential for it, which is a
	// username and a secret.
	Host     string `json:"host,omitempty"`
	Username string `json:"username,omitempty"`
	Secret   string `json:"secret,omitempty"`
}

// MaxFileSize is the largest file a profile holds. Larger files under a
// shared directory stay where they are.
const MaxFileSize = 4 << 20

// MaxProfileSize is the most a profile holds in all.
const MaxProfileSize = 64 << 20

// MaxUserPaths is how many paths a user may add.
const MaxUserPaths = 32

// DefaultPaths are shared for everyone, relative to the home directory. A
// path ending in a slash shares everything under it, but neverShared.
var DefaultPaths = []string{
	// Claude's settings, sign-in, instructions, agents, commands, skills,
	// plugins and output styles; not its sessions, history or caches.
	".claude/",
	".gitconfig",
	".config/gh/config.yml",
	".aws/config",
	".config/gcloud/configurations/",
	".config/gcloud/active_config",
	vscodeUser + "settings.json",
	vscodeUser + "keybindings.json",
	vscodeUser + "snippets/",
}

// secretPaths are the shared paths that hold credentials. Each is a whole
// file a tool writes and reads: tools that keep credentials in a database
// (gcloud's own sign-in) or in a cache they rewrite on every use (Azure's)
// cannot be shared this way.
var secretPaths = []string{
	CredentialsPath,
	".git-credentials",
	".netrc",
	".config/gh/hosts.yml",
	".config/glab-cli/config.yml",
	".docker/config.json",
	".npmrc",
	".pypirc",
	".aws/credentials",
	".kube/config",
	".config/gcloud/application_default_credentials.json",
}

func init() {
	DefaultPaths = append(DefaultPaths, secretPaths[1:]...)
}

// vscodeUser is VS Code's user settings folder: its server's data folder
// is named in its product.json.
const vscodeUser = ".vscode-server-oss/data/User/"

// CredentialsPath is Claude's sign-in.
const CredentialsPath = ".claude/.credentials.json"

// Paths is a set of shared paths: relative to the home directory for a
// profile, absolute for a pack. One ending in a slash is a directory,
// shared with everything under it; one starting with "!" is left out of
// the directory it is in, whatever else says.
type Paths []string

// Exclude is the prefix of a path left out.
const Exclude = "!"

// UserPaths is the paths a user shares: the defaults and their own, with
// what is never shared left out of the directories that hold it.
func UserPaths(own []string) Paths {
	out := append(Paths(nil), DefaultPaths...)
	for _, p := range own {
		if !slices.Contains(out, p) {
			out = append(out, p)
		}
	}
	for _, n := range neverShared {
		if _, ok := out.Covering(n); ok && !slices.Contains(out, Exclude+n) {
			out = append(out, Exclude+n)
		}
	}
	return out
}

// under reports whether p is s, or under s, a directory.
func under(p, s string) bool {
	return p == strings.TrimSuffix(s, "/") || p == s || strings.HasSuffix(s, "/") && strings.HasPrefix(p, s)
}

// Synced reports whether a path is shared: one of the paths, or under one,
// and not left out.
func (ps Paths) Synced(p string) bool {
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
		if p == s || (strings.HasSuffix(s, "/") && strings.HasPrefix(p, s)) {
			shared = true
		}
	}
	return shared
}

// Covering returns the shared path in ps, other than p itself, that
// already shares p: a shared directory it is under, or for a file, the
// file.
func (ps Paths) Covering(p string) (string, bool) {
	for _, s := range ps {
		if strings.HasPrefix(s, Exclude) {
			continue
		}
		if s == p {
			if !strings.HasSuffix(p, "/") {
				return s, true
			}
			continue
		}
		if strings.HasSuffix(s, "/") && strings.HasPrefix(p, s) {
			return s, true
		}
	}
	return "", false
}

// TrustedOnlyByDefault reports whether a path holds a credential Hangar
// knows of, which a file there is made trusted-only for (File.TrustedOnly)
// until its owner says otherwise.
func TrustedOnlyByDefault(p string) bool { return slices.Contains(secretPaths, p) }

// Valid reports whether p is a clean relative path that stays inside the
// home directory.
func Valid(p string) bool {
	return p != "" && p != "." && !strings.HasPrefix(p, "/") && path.Clean(p) == p && p != ".." &&
		!strings.HasPrefix(p, "../") && !strings.Contains(p, "\x00")
}

// ValidKey reports whether p names a file of a set: a clean path inside the
// home directory, as a profile's are, or a clean absolute one, as a pack's
// are.
func ValidKey(p string) bool {
	if strings.HasPrefix(p, "/") {
		return p != "/" && path.Clean(p) == p && !strings.Contains(p, "\x00")
	}
	return Valid(p)
}

// refusedAbsolute are where a pack may not put files: the kernel's and the
// system's own.
var refusedAbsolute = []string{"/proc/", "/sys/", "/dev/", "/run/", "/boot/"}

// CheckPackPath reports why a path cannot be one of a pack's, or nil. It is
// absolute, a directory ends in a slash, and one left out starts with "!".
func CheckPackPath(p string) error {
	p = strings.TrimPrefix(p, Exclude)
	clean := strings.TrimSuffix(p, "/")
	if !strings.HasPrefix(p, "/") || !ValidKey(clean) || strings.HasSuffix(p, "//") {
		return fmt.Errorf("%q is not a clean absolute path", p)
	}
	for _, r := range refusedAbsolute {
		if strings.HasPrefix(clean+"/", r) {
			return fmt.Errorf("%s is the system's, and cannot hold a pack's files", r)
		}
	}
	return nil
}

// neverShared are what programs rewrite all the time or keep per machine,
// and Hangar's own: never shared, and left out of any shared directory that
// holds them.
var neverShared = []string{
	".cache/",
	".local/share/hangar/",
	".vscode-server-oss/",
	".claude.json",
	".npm/",
	".cargo/registry/",
	"go/pkg/",
	// Claude's sessions, history, caches and the state it keeps per
	// machine.
	".claude/projects/",
	".claude/sessions/",
	".claude/session-env/",
	".claude/file-history/",
	".claude/shell-snapshots/",
	".claude/todos/",
	".claude/tasks/",
	".claude/jobs/",
	".claude/ide/",
	".claude/daemon/",
	".claude/daemon.log",
	".claude/statsig/",
	".claude/telemetry/",
	".claude/cache/",
	".claude/paste-cache/",
	".claude/downloads/",
	".claude/backups/",
	".claude/chrome/",
	".claude/state/",
	".claude/debug/",
	".claude/logs/",
	".claude/history.jsonl",
	".claude/stats-cache.json",
	".claude/settings.local.json",
	".claude/.last-cleanup",
	".claude/.last-update-result.json",
}

// CheckUserPath reports why a path a user asks to share, or to leave out
// ("!path"), cannot be, or nil. A directory ends in a slash.
func CheckUserPath(p string) error {
	x, exclude := strings.CutPrefix(p, Exclude)
	clean := strings.TrimSuffix(x, "/")
	if !Valid(clean) || strings.HasSuffix(x, "//") {
		return fmt.Errorf("%q is not a path inside the home directory", x)
	}
	if exclude {
		return nil
	}
	for _, n := range neverShared {
		if under(clean, n) {
			return fmt.Errorf("%s cannot be shared: it is rewritten constantly, or belongs to one machine", n)
		}
	}
	return nil
}
