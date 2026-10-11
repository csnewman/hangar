// Package fsuser runs filesystem calls as another user, as an NFS server
// does a client's: the calling thread's filesystem uid, gid and groups are
// set for the call, so the kernel checks permissions and gives new files
// their owner as it would for that user. A filesystem uid other than 0
// drops the filesystem capabilities with it, which return with 0.
package fsuser

// User is who filesystem calls are made as.
type User struct {
	UID, GID uint32
	Groups   []uint32
}

// Root is the process's own filesystem identity, for a server run as root.
var Root = User{}
