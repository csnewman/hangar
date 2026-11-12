// Package code connects the host to the Code tab's service in the guest.
//
// The service lists and watches files, answers git, and holds the files
// open for editing as shared documents. It is the agent's Rust half
// (crates/agent: `hangar-agent-rs code`, started by
// `src/codeservice.rs`), which the worker puts in each initramfs beside the
// agent. One runs per root folder, as the environment's user.
//
// # Protocol
//
// A connection on Port starts with one JSON line, a Request, naming the root.
// After it the connection is joined to that root's service as it stands:
// the service speaks length-prefixed frames, which the host carries without
// reading.
package code

// Port is the vsock port the guest agent serves the Code tab on.
const Port uint32 = 8104

// Request opens a connection.
type Request struct {
	// Root is the folder the Code tab shows. Empty is the user's home.
	Root string `json:"root,omitempty"`
}

// TrustFile is the folders whose code the service may run, written by the
// worker at each boot (api.Spec.CodeTrust): a language server starts only
// in one of them, or in a folder the person trusts from the Code tab.
const TrustFile = "/run/hangar/trusted-folders.json"
