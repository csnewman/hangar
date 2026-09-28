package editor

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"strconv"
	"strings"
	"time"

	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/guestport"
	"github.com/csnewman/hangar/internal/tunnel"
)

// An environment's own web servers are reached on hosts named
// <anything>-env<id> after Hangar's (or env<id> alone): a request that
// arrived over HTTPS goes to the environment's port 443, over TLS, and one
// over plain HTTP to its port 80, with its host, path and headers as they
// were. Hangar's own certificate is what the browser sees; the
// environment's, often self-signed, is not checked.
//
// A private environment's are reached by whoever may reach the environment,
// signed in to Hangar: the host is given a cookie of its own the way an
// editor's is. A public environment's are reached by anyone.

// portLabel ends the label of a host an environment's web servers are
// reached on, followed by the environment's ID.
const portLabel = "env"

// PortCookieName is an environment host's session cookie.
const PortCookieName = "hangar_env"

// PortsSignInPath is where on Hangar's own origin a browser is sent to sign
// in to an environment's host, with the URL it asked for as to.
func PortsSignInPath(environmentID string) string {
	return "/api/frontend/environments/" + environmentID + "/ports/sign-in"
}

// PortHost is the host an environment's web servers are reached on, which
// any name and a hyphen may go before.
func (g *Gateway) PortHost(environmentID string) string {
	return g.public.Name(portLabel + environmentID)
}

// OwnsPort reports whether a request's host is one of an environment's own
// web servers', and whose.
func (g *Gateway) OwnsPort(host string) (string, bool) {
	label, ok := g.public.Label(host)
	if !ok {
		return "", false
	}
	// An ID is hexadecimal and hyphens, so the last "env" is where it
	// starts.
	i := strings.LastIndex(label, portLabel)
	if i < 0 || (i > 0 && label[i-1] != '-') || i == 1 {
		return "", false
	}
	id := label[i+len(portLabel):]
	return id, db.ValidUUID(id)
}

// SignInPort issues a ticket for the Hangar session whose token is given and
// returns the URL on the environment's host that redeems it and goes on to
// to, an absolute URL on one of the environment's hosts. The caller has
// checked that the session's user may reach the environment.
func (g *Gateway) SignInPort(ctx context.Context, sessionToken, environmentID, to string) (string, error) {
	u, err := url.Parse(to)
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") {
		return "", fmt.Errorf("%q is not a URL on the environment's host", to)
	}
	if env, ok := g.OwnsPort(u.Host); !ok || env != environmentID {
		return "", fmt.Errorf("%q is not a URL on the environment's host", to)
	}
	ticket, err := g.sessions.Ticket(ctx, sessionToken, environmentID)
	if err != nil {
		return "", err
	}
	back := u.RequestURI()
	return u.Scheme + "://" + u.Host + signInPath + "?" + url.Values{"ticket": {ticket}, "to": {back}}.Encode(), nil
}

// ServePort serves a request for one of an environment's own web servers;
// OwnsPort has said it is one.
func (g *Gateway) ServePort(w http.ResponseWriter, r *http.Request) {
	env, _ := g.OwnsPort(r.Host)
	secure := r.TLS != nil || r.Header.Get("X-Forwarded-Proto") == "https"
	e, err := g.sessions.Environment(r.Context(), env)
	switch {
	case errors.Is(err, ErrNoSession):
		textError(w, http.StatusNotFound, "There is no such environment.")
		return
	case err != nil:
		g.log.Error("looking up an environment for its host", "err", err)
		textError(w, http.StatusInternalServerError, "Internal error.")
		return
	}
	if !e.Public {
		if !portAllowed(r, secure) {
			textError(w, http.StatusForbidden, "Cross-site request refused: this environment is private.")
			return
		}
		if r.URL.Path == signInPath {
			g.redeem(w, r, env, PortCookieName, secure)
			return
		}
		c, _ := r.Cookie(PortCookieName)
		var cookie string
		if c != nil {
			cookie = c.Value
		}
		t, err := g.sessions.Authenticate(r.Context(), cookie, env)
		switch {
		case errors.Is(err, ErrNoSession):
			if r.Method == http.MethodGet && r.Header.Get("Sec-Fetch-Mode") != "cors" {
				// A page the browser is opening: Hangar signs it in and
				// sends it back.
				scheme := "http"
				if secure {
					scheme = "https"
				}
				to := scheme + "://" + r.Host + r.URL.RequestURI()
				w.Header().Set("Cache-Control", "no-store")
				http.Redirect(w, r, g.public.Origin()+PortsSignInPath(env)+"?"+url.Values{"to": {to}}.Encode(), http.StatusSeeOther)
				return
			}
			textError(w, http.StatusUnauthorized, "This environment is private: open it in a browser signed in to Hangar.")
			return
		case err != nil:
			g.log.Error("authenticating a request to an environment's host", "err", err)
			textError(w, http.StatusInternalServerError, "Internal error.")
			return
		}
		e.Running, e.WorkerID = t.Running, t.WorkerID
	}
	if !e.Running {
		textError(w, http.StatusServiceUnavailable, "The environment is not running.")
		return
	}
	port := 80
	if secure {
		port = 443
	}
	g.portProxy.ServeHTTP(w, r.WithContext(context.WithValue(r.Context(), portTargetKey{},
		portTarget{env: env, worker: e.WorkerID, port: port, host: r.Host, secure: secure})))
}

// portAllowed refuses what another site, another environment's host among
// them, asks of a private environment's, apart from opening a page on it:
// every such host is one site with Hangar and each other, so the SameSite
// cookie alone would let one environment's pages act on another's.
func portAllowed(r *http.Request, secure bool) bool {
	switch r.Header.Get("Sec-Fetch-Site") {
	case "", "same-origin", "none":
	default:
		if r.Header.Get("Sec-Fetch-Mode") != "navigate" {
			return false
		}
	}
	scheme := "http"
	if secure {
		scheme = "https"
	}
	if origin := r.Header.Get("Origin"); origin != "" && r.Header.Get("Sec-Fetch-Mode") != "navigate" &&
		origin != scheme+"://"+r.Host {
		return false
	}
	return true
}

// redeem exchanges a sign-in ticket for a host's cookie and goes on to the
// path the ticket was issued for.
func (g *Gateway) redeem(w http.ResponseWriter, r *http.Request, env, cookieName string, secure bool) {
	q := r.URL.Query()
	cookie, err := g.sessions.Redeem(r.Context(), q.Get("ticket"), env)
	switch {
	case errors.Is(err, ErrNoSession):
		textError(w, http.StatusUnauthorized, "This link has expired. Open it again.")
		return
	case err != nil:
		g.log.Error("redeeming a sign-in ticket", "err", err)
		textError(w, http.StatusInternalServerError, "Internal error.")
		return
	}
	to := q.Get("to")
	if !strings.HasPrefix(to, "/") || strings.HasPrefix(to, "//") || strings.HasPrefix(to, "/\\") {
		to = "/"
	}
	http.SetCookie(w, &http.Cookie{
		Name:     cookieName,
		Value:    cookie,
		Path:     "/",
		MaxAge:   int(cookieLifetime.Seconds()),
		HttpOnly: true,
		Secure:   secure,
		SameSite: http.SameSiteLaxMode,
	})
	w.Header().Set("Cache-Control", "no-store")
	http.Redirect(w, r, to, http.StatusSeeOther)
}

type portTargetKey struct{}

type portTarget struct {
	env, worker string
	port        int
	// host is the one the browser asked for, which the environment is
	// told as its TLS server name too.
	host   string
	secure bool
}

func (g *Gateway) newPortProxy() *httputil.ReverseProxy {
	return &httputil.ReverseProxy{
		Rewrite:   g.portRewrite,
		Transport: g.portTransport(),
		ErrorHandler: func(w http.ResponseWriter, r *http.Request, err error) {
			t := r.Context().Value(portTargetKey{}).(portTarget)
			switch {
			case errors.Is(err, guestport.ErrRefused):
				textError(w, http.StatusBadGateway, fmt.Sprintf("Nothing in the environment is listening on port %d.", t.port))
			case errors.Is(err, tunnel.ErrNoTunnel):
				textError(w, http.StatusServiceUnavailable, "The environment's worker is not connected to Hangar just now. Reload in a moment.")
			default:
				g.log.Warn("reaching an environment's web server", "host", r.Host, "port", t.port, "err", err)
				textError(w, http.StatusBadGateway, fmt.Sprintf("The environment's port %d could not be reached: %v", t.port, err))
			}
		},
	}
}

// portRewrite sends a request on to the environment as it was made, less
// the host's own cookie: the guest has no business holding it.
func (g *Gateway) portRewrite(pr *httputil.ProxyRequest) {
	t := pr.In.Context().Value(portTargetKey{}).(portTarget)
	// The transport keeps connections per host, and dials the environment,
	// worker and port this names.
	pr.Out.URL.Scheme = "http"
	if t.port == 443 {
		pr.Out.URL.Scheme = "https"
	}
	pr.Out.URL.Host = t.env + "." + t.worker + "." + strconv.Itoa(t.port) + ".hangar-port"
	pr.Out.Host = pr.In.Host
	pr.SetXForwarded()
	if t.secure {
		pr.Out.Header.Set("X-Forwarded-Proto", "https")
	}

	var kept []string
	for _, c := range pr.Out.Cookies() {
		if c.Name != PortCookieName {
			kept = append(kept, c.String())
		}
	}
	pr.Out.Header.Del("Cookie")
	if len(kept) > 0 {
		pr.Out.Header.Set("Cookie", strings.Join(kept, "; "))
	}
}

// portTransport opens each connection to an environment's port as a stream
// through its worker's tunnel to the guest's port forwarder, and to port
// 443 over TLS, trusting whatever certificate the environment has.
func (g *Gateway) portTransport() *http.Transport {
	dial := func(ctx context.Context, addr string) (net.Conn, portTarget, error) {
		host, _, err := net.SplitHostPort(addr)
		if err != nil {
			return nil, portTarget{}, err
		}
		parts := strings.Split(host, ".")
		if len(parts) != 4 {
			return nil, portTarget{}, fmt.Errorf("not an environment port's address: %s", addr)
		}
		port, err := strconv.Atoi(parts[2])
		if err != nil {
			return nil, portTarget{}, err
		}
		conn, err := g.tunnels.Open(parts[1], tunnel.Header{Kind: tunnel.KindPorts, Environment: parts[0]})
		if err != nil {
			return nil, portTarget{}, err
		}
		if err := guestport.Request(conn, uint16(port)); err != nil {
			conn.Close()
			return nil, portTarget{}, err
		}
		t, _ := ctx.Value(portTargetKey{}).(portTarget)
		return conn, t, nil
	}
	return &http.Transport{
		DialContext: func(ctx context.Context, _, addr string) (net.Conn, error) {
			conn, _, err := dial(ctx, addr)
			return conn, err
		},
		DialTLSContext: func(ctx context.Context, _, addr string) (net.Conn, error) {
			conn, t, err := dial(ctx, addr)
			if err != nil {
				return nil, err
			}
			name := t.host
			if h, _, err := net.SplitHostPort(name); err == nil {
				name = h
			}
			tc := tls.Client(conn, &tls.Config{
				ServerName: name,
				// An environment's certificate is its own business, often
				// self-signed; the browser has already been shown Hangar's.
				InsecureSkipVerify: true,
				NextProtos:         []string{"http/1.1"},
			})
			if err := tc.HandshakeContext(ctx); err != nil {
				conn.Close()
				return nil, fmt.Errorf("TLS with the environment's port 443: %w", err)
			}
			return tc, nil
		},
		MaxIdleConnsPerHost:   16,
		IdleConnTimeout:       90 * time.Second,
		ResponseHeaderTimeout: 5 * time.Minute,
		DisableCompression:    true,
	}
}
