// Package profile is a user's profile: the files that make an environment
// theirs -- Claude's settings and sign-in, VS Code's settings, git's, and
// whatever else they choose to share -- and their SSH keys. It follows them
// into every environment they own and stays the same in all of them as they
// change it, in any environment or in the web UI.
//
// The control plane holds the profile, in Postgres. Each running
// environment's agent holds a session with the server replica its worker's
// tunnel reaches: on connecting it is sent the paths the profile shares and
// every file, which it writes into the home directory as an ordinary file,
// and from then on it sends up the changes it sees there and is sent
// everyone else's. The files are real files in the guest, written by
// renaming over, so programs watching them see a change the way they see
// any other.
//
// Two environments changing one file at once end with whichever the server
// took second. The files are small, and mostly changed in one place at a
// time.
//
// # Shared paths
//
// DefaultPaths are shared for everyone. A user adds more of their own --
// a file, or a directory and everything under it -- and removes those
// again; UserPaths is the set a user has.
//
// # Locks
//
// Some programs change a file under a lock of their own, a file or
// directory they create at a known path and remove when done. Claude is
// one: it refreshes its OAuth sign-in under ~/.claude/.oauth_refresh.lock,
// and inside it rereads .credentials.json and uses what it finds if another
// process refreshed first. A refresh token is good for one refresh, so two
// environments refreshing with it at once would sign one of them out.
//
// The server decides who holds each of the Locks. The agent serves the
// directory a lock is made in as a FUSE file system over the real one
// (LockFS), so creating the lock is a question put to the server before it
// is answered: if another environment holds it, creating it fails as it
// would if another process here held it. Taking it brings this environment's
// files up to date with the profile first, so the program finds the latest
// on disk; letting it go sends what changed under it first, so the next
// holder finds that. Only locks made of files can be served this way; a
// lock taken with flock or fcntl is the kernel's and never reaches a file
// system.
//
// # SSH keys
//
// Private keys never enter a guest. The agent serves an SSH agent socket,
// and the server signs with the key.
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
	TypePaths  = "paths"  // the paths the profile shares; sent first, and again when they change
	TypeFile   = "file"   // a file's content, or that it is gone
	TypeSynced = "synced" // every file has been sent; the agent may send its own
	TypeKeys   = "keys"   // the public keys the SSH agent offers
	TypeSigned = "signed" // the answer to a sign
	TypeLocked = "locked" // the answer to a lock or unlock

	// Agent to server.
	TypePut    = "put"    // a file changed here
	TypeDelete = "delete" // a file was removed here
	TypeLock   = "lock"   // take, renew or let go of one of the Locks
	TypeSign   = "sign"   // sign with one of the keys
)

// Message is one line of a session.
type Message struct {
	Type string `json:"type"`

	// A file: its path relative to the home directory, its content and
	// mode, and the version the server holds. Deleted marks one removed.
	Path    string `json:"path,omitempty"`
	Data    []byte `json:"data,omitempty"`
	Mode    uint32 `json:"mode,omitempty"`
	Version int64  `json:"version,omitempty"`
	Deleted bool   `json:"deleted,omitempty"`

	// Paths are the shared paths.
	Paths []string `json:"paths,omitempty"`

	// Keys are authorized-keys lines.
	Keys []string `json:"keys,omitempty"`

	// Files come with a granted lock: the profile's files in the lock's
	// directory, which its holder must find on disk.
	Files []Message `json:"files,omitempty"`

	// Held asks to hold the lock at Path, or not; in an answer, whether it
	// is held. A lock asked for with no ID is a renewal, and has no answer.
	Held bool `json:"held,omitempty"`

	// A request and its answer share an ID. For a signature, Key is the
	// public key in SSH wire form and Signature an ssh.Signature,
	// marshalled.
	ID        int64  `json:"id,omitempty"`
	Key       []byte `json:"key,omitempty"`
	Flags     uint32 `json:"flags,omitempty"`
	Signature []byte `json:"signature,omitempty"`
	Error     string `json:"error,omitempty"`
}

// MaxFileSize is the largest file a profile holds. Larger files under a
// shared directory stay where they are.
const MaxFileSize = 4 << 20

// MaxProfileSize is the most a profile holds in all.
const MaxProfileSize = 64 << 20

// MaxUserPaths is how many paths a user may add.
const MaxUserPaths = 32

// DefaultPaths are shared for everyone, relative to the home directory. A
// path ending in a slash shares everything under it.
var DefaultPaths = []string{
	".claude/settings.json",
	".claude/CLAUDE.md",
	CredentialsPath,
	".claude/agents/",
	".claude/commands/",
	".claude/skills/",
	".claude/output-styles/",
	".gitconfig",
	vscodeUser + "settings.json",
	vscodeUser + "keybindings.json",
	vscodeUser + "snippets/",
}

// vscodeUser is VS Code's user settings folder: its server's data folder
// is named in its product.json.
const vscodeUser = ".vscode-server-oss/data/User/"

// CredentialsPath is Claude's sign-in.
const CredentialsPath = ".claude/.credentials.json"

// Lock is a lock a program takes by creating a path.
type Lock struct {
	// Path is relative to the home directory. Its directory is served by
	// LockFS.
	Path string
}

// Locks are the locks the server decides.
var Locks = []Lock{
	// Claude takes this with proper-lockfile: mkdir, touched every five
	// seconds, stale after sixty.
	{Path: ".claude/.oauth_refresh.lock"},
}

// LockAt returns the lock at a path.
func LockAt(p string) (Lock, bool) {
	for _, l := range Locks {
		if l.Path == p {
			return l, true
		}
	}
	return Lock{}, false
}

// LockDirs are the directories LockFS serves: those the Locks are made in.
func LockDirs() []string {
	var out []string
	for _, l := range Locks {
		if d := path.Dir(l.Path); !slices.Contains(out, d) {
			out = append(out, d)
		}
	}
	return out
}

// Paths is a set of shared paths, relative to the home directory. One
// ending in a slash is a directory, shared with everything under it.
type Paths []string

// UserPaths is the paths a user shares: the defaults and their own.
func UserPaths(own []string) Paths {
	out := append(Paths(nil), DefaultPaths...)
	for _, p := range own {
		if !slices.Contains(out, p) {
			out = append(out, p)
		}
	}
	return out
}

// Synced reports whether a path is shared.
func (ps Paths) Synced(p string) bool {
	if !Valid(p) {
		return false
	}
	for _, s := range ps {
		if p == s || (strings.HasSuffix(s, "/") && strings.HasPrefix(p, s)) {
			return true
		}
	}
	return false
}

// Dirs are the directories the agent watches: the parents of shared files,
// whose other contents are not shared, and the shared directories, watched
// with everything under them.
func (ps Paths) Dirs() (parents, trees []string) {
	for _, p := range ps {
		if strings.HasSuffix(p, "/") {
			trees = append(trees, strings.TrimSuffix(p, "/"))
		} else if d := path.Dir(p); !slices.Contains(parents, d) {
			parents = append(parents, d)
		}
	}
	return parents, trees
}

// InTree reports whether a directory is one of the shared directories or
// under one.
func (ps Paths) InTree(dir string) bool {
	for _, s := range ps {
		if strings.HasSuffix(s, "/") && strings.HasPrefix(dir+"/", s) {
			return true
		}
	}
	return false
}

// Secret reports whether a path holds a credential: kept encrypted, never
// shown back in the web UI, and not sent to environments that are not
// trusted with the owner's credentials.
func Secret(p string) bool { return p == CredentialsPath }

// Valid reports whether p is a clean relative path that stays inside the
// home directory.
func Valid(p string) bool {
	return p != "" && p != "." && !strings.HasPrefix(p, "/") && path.Clean(p) == p && p != ".." &&
		!strings.HasPrefix(p, "../") && !strings.Contains(p, "\x00")
}

// refused are places a user may not share: what programs rewrite all the
// time or hold per machine, and Hangar's own.
var refused = []string{
	".cache/",
	".local/share/hangar/",
	".vscode-server-oss/",
	".claude/projects/",
	".claude/todos/",
	".claude/shell-snapshots/",
	".claude/statsig/",
	".claude/ide/",
	".claude.json",
	".npm/",
	".cargo/registry/",
	"go/pkg/",
}

// CheckUserPath reports why a path a user asks to share cannot be, or nil.
// A directory ends in a slash.
func CheckUserPath(p string) error {
	clean := strings.TrimSuffix(p, "/")
	if !Valid(clean) || strings.HasSuffix(p, "//") {
		return fmt.Errorf("%q is not a path inside the home directory", p)
	}
	for _, r := range refused {
		isDir := strings.HasSuffix(r, "/")
		switch {
		case clean == strings.TrimSuffix(r, "/"),
			isDir && strings.HasPrefix(p, r),
			strings.HasSuffix(p, "/") && strings.HasPrefix(r, p):
			return fmt.Errorf("%s cannot be shared: it is rewritten constantly, or belongs to one machine", r)
		}
	}
	return nil
}
