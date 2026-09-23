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
	"net/http"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/server"
	"github.com/csnewman/hangar/internal/webui"
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

	srv, err := server.New(server.Config{DB: d, BootstrapToken: token, Web: web,
		PublicURL: os.Getenv("HANGAR_PUBLIC_URL"), AutoSignIn: autoSignIn})
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

	hs := &http.Server{
		Addr:              listen,
		Handler:           srv.Handler(),
		ReadHeaderTimeout: 10 * time.Second,
	}
	errc := make(chan error, 1)
	go func() { errc <- hs.ListenAndServe() }()
	slog.Info("serving", "addr", listen)

	select {
	case err := <-errc:
		return err
	case <-ctx.Done():
	}
	slog.Info("shutting down")
	shutdown, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	return hs.Shutdown(shutdown)
}

func envOr(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}
