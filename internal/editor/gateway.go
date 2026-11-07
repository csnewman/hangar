package editor

import (
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"net/http/httputil"
	"regexp"

	"github.com/csnewman/hangar/internal/publichost"
	"github.com/csnewman/hangar/internal/tunnel"
)

// signInPath is where an environment's host redeems a ticket, a path its own
// web servers are unlikely to use.
const signInPath = "/_hangar/sign-in"

// shortID is an environment's short ID, which names its hosts.
var shortID = regexp.MustCompile(`^[a-z0-9]{6}$`)

// Tunnels opens streams to workers.
type Tunnels interface {
	Open(workerID string, h tunnel.Header) (net.Conn, error)
}

// Gateway serves every environment's own web servers, each environment on
// hosts of its own named after Hangar's public one (ports.go).
type Gateway struct {
	sessions *Manager
	tunnels  Tunnels
	// public is Hangar's own origin, which names the environments' hosts.
	public *publichost.Public
	// portProxy reaches environments' own web servers.
	portProxy *httputil.ReverseProxy
	log       *slog.Logger
}

// NewGateway serves environments' web servers on hosts named after public,
// Hangar's own origin as the browser sees it. Those names are in Hangar's
// own site, so a host's cookie is sent with a page Hangar opens on it.
func NewGateway(sessions *Manager, tunnels Tunnels, public *publichost.Public, log *slog.Logger) *Gateway {
	g := &Gateway{sessions: sessions, tunnels: tunnels, public: public, log: log}
	g.portProxy = g.newPortProxy()
	return g
}

func textError(w http.ResponseWriter, code int, msg string) {
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(code)
	fmt.Fprintln(w, msg)
}
