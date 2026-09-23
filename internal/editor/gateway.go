package editor

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"strings"
	"time"

	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/tunnel"
)

// CookieName is the editor origin's session cookie.
const CookieName = "hangar_editor"

// signInPath is where the editor origin redeems a ticket. It is outside any
// path VS Code's server uses.
const signInPath = "/_hangar/sign-in"

// hostPrefix begins every editor origin's host name.
const hostPrefix = "e-"

// Tunnels opens streams to workers.
type Tunnels interface {
	Open(workerID string, h tunnel.Header) (net.Conn, error)
}

// Gateway serves every environment's editor, each on its own host under
// Hangar's public one.
type Gateway struct {
	sessions *Manager
	tunnels  Tunnels
	// public is Hangar's own origin: an editor's host is a subdomain of its
	// host, and only it may frame the editor.
	public *url.URL
	proxy  *httputil.ReverseProxy
	log    *slog.Logger
}

// NewGateway serves editors under public, Hangar's own origin as the browser
// sees it, such as https://hangar.example.com.
//
// Editor hosts must be subdomains of Hangar's host so that the two are one
// site: the browser then sends the editor origin its cookie inside Hangar's
// iframe, which it may refuse for a cross-site frame.
func NewGateway(sessions *Manager, tunnels Tunnels, public string, log *slog.Logger) (*Gateway, error) {
	u, err := url.Parse(public)
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" {
		return nil, fmt.Errorf("the public URL must be an http or https origin, such as https://hangar.example.com; got %q", public)
	}
	g := &Gateway{sessions: sessions, tunnels: tunnels, public: &url.URL{Scheme: u.Scheme, Host: u.Host}, log: log}
	g.proxy = &httputil.ReverseProxy{
		Rewrite:        g.rewrite,
		Transport:      g.transport(),
		ModifyResponse: g.modifyResponse,
		ErrorHandler: func(w http.ResponseWriter, r *http.Request, err error) {
			g.log.Warn("reaching an editor", "host", r.Host, "err", err)
			if errors.Is(err, tunnel.ErrNoTunnel) {
				textError(w, http.StatusServiceUnavailable, "The environment's worker is not connected to Hangar just now. Reload in a moment.")
				return
			}
			textError(w, http.StatusBadGateway, "The editor could not be reached. It may still be starting; reload to try again.")
		},
	}
	return g, nil
}

// Owns reports whether a request's host is an editor's, and whose.
func (g *Gateway) Owns(host string) (string, bool) {
	suffix := "." + g.public.Host
	if !strings.HasPrefix(host, hostPrefix) || !strings.HasSuffix(host, suffix) {
		return "", false
	}
	id := strings.TrimSuffix(strings.TrimPrefix(host, hostPrefix), suffix)
	return id, db.ValidUUID(id)
}

// Origin is one environment's editor origin.
func (g *Gateway) Origin(environmentID string) string {
	return g.public.Scheme + "://" + hostPrefix + environmentID + "." + g.public.Host
}

// SignIn issues a ticket for the Hangar session whose token is given and
// returns the URL that redeems it and opens the editor on folder, which may
// be empty. The caller has checked that the session's user may reach the
// environment.
func (g *Gateway) SignIn(ctx context.Context, sessionToken, environmentID, folder string) (string, error) {
	ticket, err := g.sessions.Ticket(ctx, sessionToken, environmentID)
	if err != nil {
		return "", err
	}
	to := "/"
	if folder != "" {
		to += "?" + url.Values{"folder": {folder}}.Encode()
	}
	return g.Origin(environmentID) + signInPath + "?" + url.Values{"ticket": {ticket}, "to": {to}}.Encode(), nil
}

// ServeHTTP serves a request for an editor host; Owns has said it is one.
func (g *Gateway) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	env, _ := g.Owns(r.Host)
	if !g.allowed(r, env) {
		textError(w, http.StatusForbidden, "Cross-origin request refused.")
		return
	}
	if r.URL.Path == signInPath {
		g.signIn(w, r, env)
		return
	}

	c, _ := r.Cookie(CookieName)
	var cookie string
	if c != nil {
		cookie = c.Value
	}
	t, err := g.sessions.Authenticate(r.Context(), cookie, env)
	switch {
	case errors.Is(err, ErrNoSession):
		textError(w, http.StatusUnauthorized, "Open this environment's editor from Hangar.")
		return
	case err != nil:
		g.log.Error("authenticating an editor request", "err", err)
		textError(w, http.StatusInternalServerError, "Internal error.")
		return
	case !t.Running:
		textError(w, http.StatusConflict, "The environment is not running.")
		return
	}
	g.proxy.ServeHTTP(w, r.WithContext(context.WithValue(r.Context(), targetKey{}, target{env: env, worker: t.WorkerID})))
}

// allowed refuses what another site, or another environment's editor, asks
// of this one. Every editor host is one site with Hangar and with each
// other, so the SameSite cookie alone would let one environment's editor
// drive another's; only Hangar's page opening the editor in its iframe, and
// the editor's own requests, get through.
func (g *Gateway) allowed(r *http.Request, env string) bool {
	switch r.Header.Get("Sec-Fetch-Site") {
	case "", "same-origin", "none":
	case "same-site":
		// Hangar's iframe, or a link the user followed.
		if r.Header.Get("Sec-Fetch-Mode") != "navigate" {
			return false
		}
	default:
		return false
	}
	// The browser sends Origin on a WebSocket's opening request, which
	// Sec-Fetch-Site does not cover in every browser.
	if origin := r.Header.Get("Origin"); origin != "" && origin != g.Origin(env) {
		return false
	}
	return true
}

func (g *Gateway) signIn(w http.ResponseWriter, r *http.Request, env string) {
	q := r.URL.Query()
	cookie, err := g.sessions.Redeem(r.Context(), q.Get("ticket"), env)
	switch {
	case errors.Is(err, ErrNoSession):
		textError(w, http.StatusUnauthorized, "This link has expired. Open the editor from Hangar again.")
		return
	case err != nil:
		g.log.Error("redeeming an editor ticket", "err", err)
		textError(w, http.StatusInternalServerError, "Internal error.")
		return
	}
	to := q.Get("to")
	if !strings.HasPrefix(to, "/") || strings.HasPrefix(to, "//") || strings.HasPrefix(to, "/\\") {
		to = "/"
	}
	http.SetCookie(w, &http.Cookie{
		Name:     CookieName,
		Value:    cookie,
		Path:     "/",
		MaxAge:   int(cookieLifetime.Seconds()),
		HttpOnly: true,
		Secure:   g.public.Scheme == "https",
		SameSite: http.SameSiteLaxMode,
	})
	w.Header().Set("Cache-Control", "no-store")
	http.Redirect(w, r, to, http.StatusSeeOther)
}

type targetKey struct{}

type target struct {
	env    string
	worker string
}

// rewrite sends a request on to the editor as the browser made it, less the
// editor origin's own cookie: the guest has no business holding it. The
// Host header is kept, since VS Code's server builds the URLs it hands the
// browser from it.
func (g *Gateway) rewrite(pr *httputil.ProxyRequest) {
	t := pr.In.Context().Value(targetKey{}).(target)
	// The transport keeps connections per host, and dials the environment
	// and worker this names.
	pr.Out.URL.Scheme = "http"
	pr.Out.URL.Host = t.env + "." + t.worker + ".hangar-editor"
	pr.Out.Host = pr.In.Host
	pr.SetXForwarded()
	pr.Out.Header.Set("X-Forwarded-Proto", g.public.Scheme)

	var kept []string
	for _, c := range pr.Out.Cookies() {
		if c.Name != CookieName {
			kept = append(kept, c.String())
		}
	}
	pr.Out.Header.Del("Cookie")
	if len(kept) > 0 {
		pr.Out.Header.Set("Cookie", strings.Join(kept, "; "))
	}
}

// transport opens each connection to an editor as a stream through its
// worker's tunnel.
func (g *Gateway) transport() *http.Transport {
	return &http.Transport{
		DialContext: func(ctx context.Context, _, addr string) (net.Conn, error) {
			host, _, err := net.SplitHostPort(addr)
			if err != nil {
				return nil, err
			}
			parts := strings.Split(host, ".")
			if len(parts) != 3 {
				return nil, fmt.Errorf("not an editor address: %s", addr)
			}
			return g.tunnels.Open(parts[1], tunnel.Header{Kind: tunnel.KindEditor, Environment: parts[0]})
		},
		MaxIdleConnsPerHost:   16,
		IdleConnTimeout:       90 * time.Second,
		ResponseHeaderTimeout: 2 * time.Minute,
		// The browser negotiates compression with VS Code's server itself.
		DisableCompression: true,
	}
}

// modifyResponse lets only Hangar's own page frame the editor, and the
// editor frame itself: VS Code runs its web worker extension host in an
// iframe of its own origin, and every ancestor of a frame must be allowed.
func (g *Gateway) modifyResponse(resp *http.Response) error {
	resp.Header.Add("Content-Security-Policy", "frame-ancestors 'self' "+g.public.String())
	return nil
}

func textError(w http.ResponseWriter, code int, msg string) {
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(code)
	fmt.Fprintln(w, msg)
}
