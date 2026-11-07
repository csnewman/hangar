package frontendapi

import "context"

// Hosts signs users in to the hosts environments' own web servers are
// reached on.
type Hosts interface {
	// SignInPort returns the URL that signs the holder of a Hangar session
	// in to the host of an environment's own web server that to is on, and
	// goes on to it.
	SignInPort(ctx context.Context, sessionToken, environmentID, short, to string) (string, error)
	// PortHost is the host the web servers of the environment with this
	// short ID are reached on, with any name and a hyphen before it.
	PortHost(short string) string
}
