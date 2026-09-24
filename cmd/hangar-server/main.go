// Command hangar-server is the Hangar control plane: the API, the web UI,
// placement, and the only program that touches the database.
//
// Any number of replicas may run against one database. They share nothing
// but it.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/csnewman/hangar/internal/certs"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/server"
	"github.com/csnewman/hangar/internal/webui"
	"github.com/csnewman/hangar/internal/zone"
)

func main() {
	listen := flag.String("listen", envOr("HANGAR_LISTEN", ":8081"), "address to serve on")
	dbURL := flag.String("database", os.Getenv("HANGAR_DATABASE_URL"), "Postgres connection URL")
	tokenFile := flag.String("bootstrap-token-file", os.Getenv("HANGAR_BOOTSTRAP_TOKEN_FILE"),
		"file holding the token workers register with")
	debug := flag.Bool("debug", false, "log at debug level")
	migrateOnly := flag.Bool("migrate-only", false, "bring the schema up to date, then exit without serving")
	flag.Parse()

	level := slog.LevelInfo
	if *debug {
		level = slog.LevelDebug
	}
	slog.SetDefault(slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: level})))

	if err := run(*listen, *dbURL, *tokenFile, *migrateOnly); err != nil {
		fmt.Fprintf(os.Stderr, "hangar-server: %v\n", err)
		os.Exit(1)
	}
}

func run(listen, dbURL, tokenFile string, migrateOnly bool) error {
	if dbURL == "" {
		return errors.New("no database: pass -database or set HANGAR_DATABASE_URL")
	}

	// HANGAR_BOOTSTRAP_TOKEN is for development; a file keeps the token out
	// of the process environment, where anything that can read /proc can see
	// it.
	token := os.Getenv("HANGAR_BOOTSTRAP_TOKEN")
	if tokenFile != "" {
		b, err := os.ReadFile(tokenFile)
		if err != nil {
			return fmt.Errorf("reading the bootstrap token: %w", err)
		}
		token = strings.TrimSpace(string(b))
	}
	if token == "" {
		slog.Warn("no bootstrap token; workers cannot register")
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	d, err := db.Open(ctx, dbURL)
	if err != nil {
		return err
	}
	defer d.Close()
	if migrateOnly {
		slog.Info("schema up to date")
		return nil
	}

	web := webui.FS()
	if web == nil {
		slog.Warn("the web UI was not built into this binary")
	}

	// HANGAR_DEV_AUTO_SIGN_IN signs every visitor in as that user, with no
	// password: for a development stack on one machine, never anything else.
	autoSignIn := os.Getenv("HANGAR_DEV_AUTO_SIGN_IN")
	if autoSignIn != "" {
		slog.Warn("AUTHENTICATION IS OFF: every visitor is signed in as this user (HANGAR_DEV_AUTO_SIGN_IN)", "username", autoSignIn)
	}

	// Users' credentials and SSH keys are encrypted with this key.
	var sealer *profile.Sealer
	if f := os.Getenv("HANGAR_SECRET_KEY_FILE"); f != "" {
		if sealer, err = profile.LoadSealer(f); err != nil {
			return err
		}
	} else {
		slog.Warn("no secret key (HANGAR_SECRET_KEY_FILE); users cannot keep credentials or SSH keys")
	}

	srv, err := server.New(server.Config{DB: d, BootstrapToken: token, Web: web,
		PublicURL: os.Getenv("HANGAR_PUBLIC_URL"), AutoSignIn: autoSignIn, Sealer: sealer})
	if err != nil {
		return err
	}

	// A new installation has no users, and so nobody who could create one.
	// The first administrator comes from the environment, and only while the
	// users table is empty; afterwards these are ignored.
	if pw := os.Getenv("HANGAR_INITIAL_ADMIN_PASSWORD"); pw != "" {
		name := envOr("HANGAR_INITIAL_ADMIN_USERNAME", "admin")
		created, err := srv.Users().EnsureAdmin(ctx, name, pw)
		if err != nil {
			return fmt.Errorf("creating the initial administrator: %w", err)
		}
		if created {
			slog.Info("created the initial administrator", "username", name)
		}
	}
	go srv.Run(ctx)

	errc := make(chan error, 4)
	servers := []*http.Server{{
		Addr:              listen,
		Handler:           srv.Handler(),
		ReadHeaderTimeout: 10 * time.Second,
	}}
	go func() { errc <- servers[0].ListenAndServe() }()
	slog.Info("serving HTTP", "addr", listen)

	edge, err := serveEdge(ctx, d, srv.Handler(), errc)
	if err != nil {
		return err
	}
	servers = append(servers, edge...)

	select {
	case err := <-errc:
		return err
	case <-ctx.Done():
	}
	slog.Info("shutting down")
	shutdown, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	for _, hs := range servers {
		if err := hs.Shutdown(shutdown); err != nil {
			return err
		}
	}
	return nil
}

// serveEdge starts what faces the internet directly, each part only when
// configured: the zone's DNS server (HANGAR_DNS_LISTEN), HTTPS with a
// certificate from ACME (HANGAR_TLS_LISTEN), and a redirect from HTTP to it
// (HANGAR_REDIRECT_LISTEN). The zone is the host of HANGAR_PUBLIC_URL.
func serveEdge(ctx context.Context, d *db.DB, handler http.Handler, errc chan<- error) ([]*http.Server, error) {
	dnsListen := os.Getenv("HANGAR_DNS_LISTEN")
	tlsListen := os.Getenv("HANGAR_TLS_LISTEN")
	redirectListen := os.Getenv("HANGAR_REDIRECT_LISTEN")
	if dnsListen == "" && tlsListen == "" && redirectListen == "" {
		return nil, nil
	}
	public, err := url.Parse(os.Getenv("HANGAR_PUBLIC_URL"))
	if err != nil || public.Hostname() == "" {
		return nil, errors.New("HANGAR_DNS_LISTEN, HANGAR_TLS_LISTEN and HANGAR_REDIRECT_LISTEN need HANGAR_PUBLIC_URL, whose host is the zone")
	}
	host := public.Hostname()
	records := zone.NewManager(d)

	if dnsListen != "" {
		var addrs []netip.Addr
		for _, a := range splitList(os.Getenv("HANGAR_DNS_ADDRESSES")) {
			ip, err := netip.ParseAddr(a)
			if err != nil {
				return nil, fmt.Errorf("HANGAR_DNS_ADDRESSES: %w", err)
			}
			addrs = append(addrs, ip)
		}
		dns, err := zone.NewServer(zone.Config{
			Zone:        host,
			Addresses:   addrs,
			Nameservers: splitList(os.Getenv("HANGAR_DNS_NAMESERVERS")),
			Hostmaster:  os.Getenv("HANGAR_DNS_HOSTMASTER"),
		}, records, slog.Default())
		if err != nil {
			return nil, err
		}
		go func() {
			if err := dns.Serve(ctx, dnsListen); err != nil {
				errc <- fmt.Errorf("serving DNS: %w", err)
			}
		}()
	}

	// A challenge's records are removed once it is done; any a replica left
	// behind by dying midway are pruned.
	go func() {
		t := time.NewTicker(10 * time.Minute)
		defer t.Stop()
		for {
			if n, err := records.Prune(ctx, time.Hour); err != nil && ctx.Err() == nil {
				slog.Warn("pruning DNS records", "err", err)
			} else if n > 0 {
				slog.Info("pruned stale DNS records", "count", n)
			}
			select {
			case <-ctx.Done():
				return
			case <-t.C:
			}
		}
	}()

	var servers []*http.Server
	if tlsListen != "" {
		if public.Scheme != "https" {
			return nil, errors.New("HANGAR_TLS_LISTEN needs an https HANGAR_PUBLIC_URL")
		}
		o := certs.Options{Zone: host, Email: os.Getenv("HANGAR_ACME_EMAIL"), CA: os.Getenv("HANGAR_ACME_CA")}
		if f := os.Getenv("HANGAR_ACME_CA_ROOTS"); f != "" {
			if o.CARoots, err = os.ReadFile(f); err != nil {
				return nil, fmt.Errorf("HANGAR_ACME_CA_ROOTS: %w", err)
			}
		}
		cm, err := certs.New(d, records, o, slog.Default())
		if err != nil {
			return nil, err
		}
		if err := cm.Start(ctx); err != nil {
			return nil, err
		}
		hs := &http.Server{
			Addr:              tlsListen,
			Handler:           handler,
			TLSConfig:         cm.TLSConfig(),
			ReadHeaderTimeout: 10 * time.Second,
		}
		go func() { errc <- hs.ListenAndServeTLS("", "") }()
		slog.Info("serving HTTPS", "addr", tlsListen)
		servers = append(servers, hs)
	}

	if redirectListen != "" {
		hs := &http.Server{
			Addr: redirectListen,
			Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				to := *public
				to.Host = r.Host
				if h, _, err := net.SplitHostPort(r.Host); err == nil {
					to.Host = h
				}
				if public.Port() != "" {
					to.Host = net.JoinHostPort(to.Host, public.Port())
				}
				to.Path, to.RawQuery = r.URL.Path, r.URL.RawQuery
				http.Redirect(w, r, to.String(), http.StatusPermanentRedirect)
			}),
			ReadHeaderTimeout: 10 * time.Second,
		}
		go func() { errc <- hs.ListenAndServe() }()
		slog.Info("redirecting HTTP to HTTPS", "addr", redirectListen)
		servers = append(servers, hs)
	}
	return servers, nil
}

func splitList(s string) []string {
	var out []string
	for _, f := range strings.FieldsFunc(s, func(r rune) bool { return r == ',' || r == ' ' }) {
		out = append(out, f)
	}
	return out
}

func envOr(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}
