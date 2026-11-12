// Package sshd is what the host knows of the SSH server inside an
// environment, which Hangar's SSH gateway reaches it through: `ssh
// <environment>@<hangar> -p 2222`, and an editor's remote SSH over the
// same. The server is the guest agent's (crates/agent/src/sshd.rs); tests
// stand in for it with sshdtest.
package sshd

// Port is the vsock port the guest agent serves SSH on.
// It is below 1024, as every port the agent listens on is (internal/agent).
const Port uint32 = 107
