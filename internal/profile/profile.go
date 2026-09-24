// Package profile is a user's profile: the files that make an environment
// theirs -- Claude's settings and sign-in, VS Code's settings, git's -- and
// their SSH keys. It follows them into every environment they own and stays
// the same in all of them as they change it, in any environment or in the
// web UI.
//
// The control plane holds the profile, in Postgres. Each running
// environment's agent holds a session with the server replica its worker's
// tunnel reaches: on connecting it is sent every file, which it writes into
// the home directory as an ordinary file, and from then on it sends up the
// changes it sees there and is sent everyone else's. The files are real
// files in the guest, written by renaming over, so programs watching them
// see a change the way they see any other.
//
// Two environments changing one file at once end with whichever the server
// took second. The files are small, and mostly changed in one place at a
// time.
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
// Locks lists such locks, and each is carried between environments: when a
// program takes it in one, the server has every other environment's agent
// take it there for as long as it is held, and the files changed under it
// reach them before they let it go. A program waiting on it there then finds
// the change on disk, as it would after another process on its own machine.
// Only locks made of files can be carried; a lock taken with flock or fcntl
// is the kernel's and cannot be seen from outside the process.
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
	"path"
	"strings"
)

// Port is the vsock port the guest agent serves profile sessions on.
const Port uint32 = 8105

// Message types.
const (
	// Server to agent.
	TypeFile   = "file"   // a file's content, or that it is gone
	TypeSynced = "synced" // every file has been sent; the agent may send its own
	TypeKeys   = "keys"   // the public keys the SSH agent offers
	TypeLock   = "lock"   // hold or release one of the Locks here
	TypeSigned = "signed" // the answer to a sign

	// Agent to server.
	TypePut    = "put"    // a file changed here
	TypeDelete = "delete" // a file was removed here
	TypeLocked = "locked" // a program took or released one of the Locks here
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

	// Keys are authorized-keys lines.
	Keys []string `json:"keys,omitempty"`

	// Held is whether the lock at Path is held.
	Held bool `json:"held,omitempty"`

	// A signature request and its answer. Key is the public key in SSH wire
	// form; Signature is ssh.Signature, marshalled.
	ID        int64  `json:"id,omitempty"`
	Key       []byte `json:"key,omitempty"`
	Flags     uint32 `json:"flags,omitempty"`
	Signature []byte `json:"signature,omitempty"`
	Error     string `json:"error,omitempty"`
}

// MaxFileSize is the largest file a profile holds.
const MaxFileSize = 1 << 20

// Paths a profile holds, relative to the home directory. A path ending in
// a slash holds everything under it.
var synced = []string{
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

// Paths are what a profile holds, relative to the home directory. One
// ending in a slash holds everything under it.
func Paths() []string { return append([]string(nil), synced...) }

// Dirs are the directories the agent watches, relative to the home
// directory: the parents of synced files, whose other contents are not
// part of the profile, and the synced directories, watched with everything
// under them.
func Dirs() (parents, trees []string) {
	seen := map[string]bool{}
	for _, p := range synced {
		if strings.HasSuffix(p, "/") {
			trees = append(trees, strings.TrimSuffix(p, "/"))
			continue
		}
		if d := path.Dir(p); !seen[d] {
			seen[d] = true
			parents = append(parents, d)
		}
	}
	return parents, trees
}

// InTree reports whether a directory, relative to the home directory, is
// one of the synced directories or under one.
func InTree(dir string) bool {
	for _, s := range synced {
		if strings.HasSuffix(s, "/") && strings.HasPrefix(dir+"/", s) {
			return true
		}
	}
	return false
}

// vscodeUser is VS Code's user settings folder: its server's data folder
// is named in its product.json.
const vscodeUser = ".vscode-server-oss/data/User/"

// CredentialsPath is Claude's sign-in.
const CredentialsPath = ".claude/.credentials.json"

// Lock is a lock a program takes by creating a path, and holds by keeping
// its modification time fresh.
type Lock struct {
	// Path is relative to the home directory.
	Path string
	// Dir is whether the lock is a directory rather than a file.
	Dir bool
}

// Locks are the locks carried between environments.
var Locks = []Lock{
	// Claude takes this with proper-lockfile: mkdir, touched every five
	// seconds, stale after sixty.
	{Path: ".claude/.oauth_refresh.lock", Dir: true},
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

// Synced reports whether a path, relative to the home directory, is part of
// a profile.
func Synced(p string) bool {
	if !Valid(p) {
		return false
	}
	for _, s := range synced {
		if p == s || (strings.HasSuffix(s, "/") && strings.HasPrefix(p, s)) {
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
	return p != "" && !strings.HasPrefix(p, "/") && path.Clean(p) == p && p != ".." &&
		!strings.HasPrefix(p, "../") && !strings.Contains(p, "\x00")
}
