package nfs

// View is what one environment may reach: its file sets, each a directory
// in the server's root named by the set's ID, and everything in them but
// the files it is not trusted with.
type View interface {
	// Sets are the IDs of the file sets the environment is given.
	Sets() []string
	// Hidden reports whether a file in a set, by its path in the set, is
	// kept from the environment.
	Hidden(set, path string) bool
	// Squash is who a set's callers act as.
	Squash(set string) Squash
}

// Squash is who a set's callers act as, as an NFS export's options say.
type Squash int

const (
	// SquashAll takes every caller as the server's user (Config.UID and
	// GID), root among them; changing an owner is accepted and changes
	// nothing.
	SquashAll Squash = iota
	// SquashRoot takes root as nobody, and every other caller as itself.
	SquashRoot
	// SquashNone takes every caller as itself, root included.
	SquashNone
)
