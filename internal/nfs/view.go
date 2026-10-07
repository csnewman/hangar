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
}
