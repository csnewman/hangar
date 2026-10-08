// Package profile is what of a user follows them into their environments
// beside the files: their SSH keys and registry credentials, carried over
// a session the server holds with each environment's agent, which also
// tells the agent the copies of packs it routes paths to (internal/packs)
// and hears back what it routed and found.
//
// # Shared files
//
// The server sends the copies an environment uses and their paths. The
// agent routes the paths there (hangarfs, to the worker's NFS server).
// Before it changes the routes it sorts each path whose sharing changes:
// a file only the environment has is copied up into the copy; one only
// the copy has simply appears; a file the environment had of its own,
// differing from the copy's, is kept aside and reported as a conflict,
// which the server resolves; and a path that stops being shared is copied
// down, so the environment keeps it as its own.
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
// whose spec has NoRegistry is sent none.
//
// # Protocol
//
// A session is JSON lines both ways on one stream, which the server opens
// to the agent's Port. Each line is a Message.
package profile

// Port is the vsock port the guest agent serves profile sessions on.
const Port uint32 = 8105

// Message types.
const (
	// Server to agent.
	TypePaths  = "paths"  // the copies the environment uses and their paths; again when they change
	TypeKeys   = "keys"   // the public keys the SSH agent offers
	TypeSigned = "signed" // the answer to a sign
	// How a conflict is resolved: Copy, Path and Resolution
	// (ResolveShared or ResolveEnvironment).
	TypeResolve = "resolve"
	// Hangar's registry's host, sent first when there is one and the
	// environment's spec does not have NoRegistry.
	TypeRegistry   = "registry"
	TypeCredential = "credential" // the answer to a get_credential

	// Agent to server.
	TypeSign = "sign" // sign with one of the keys
	// The copies the agent routes to, once it has applied a TypePaths.
	TypeRouted = "routed"
	// A conflict found (Copy and Path), and one resolved.
	TypeConflict = "conflict"
	TypeResolved = "resolved"
	// A credential for Hangar's registry, as the environment's owner.
	TypeGetCredential = "get_credential"
)

// Message is one line of a session.
type Message struct {
	Type string `json:"type"`

	// Sets are the copies the environment uses, in order, with the paths
	// each shares; Routed the IDs of those the agent routes to.
	Sets   []SetPaths `json:"sets,omitempty"`
	Routed []string   `json:"routed,omitempty"`

	// A conflict: the copy, the path (as a pack names it) and how it is
	// resolved.
	Copy       string `json:"copy,omitempty"`
	Path       string `json:"path,omitempty"`
	Resolution string `json:"resolution,omitempty"`

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

// How a conflict is resolved.
const (
	ResolveShared      = "shared"      // the copy's file stays; the environment's is dropped
	ResolveEnvironment = "environment" // the environment's file replaces the copy's
)
