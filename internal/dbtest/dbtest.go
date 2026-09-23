// Package dbtest gives a test a database of its own.
//
// Tests that need Postgres run only when HANGAR_TEST_DATABASE_URL names a
// server they may create databases on, and are skipped otherwise. The
// development stack publishes one:
//
//	HANGAR_TEST_DATABASE_URL=postgres://hangar:hangar@localhost:55432/hangar?sslmode=disable go test ./...
package dbtest

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"net/url"
	"os"
	"testing"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/db"
)

// URL returns the URL of a fresh, empty database that is dropped when the
// test ends, or skips the test if there is no server to create it on.
func URL(t testing.TB) string {
	t.Helper()
	admin := os.Getenv("HANGAR_TEST_DATABASE_URL")
	if admin == "" {
		t.Skip("HANGAR_TEST_DATABASE_URL is not set")
	}
	ctx := context.Background()

	conn, err := pgx.Connect(ctx, admin)
	if err != nil {
		t.Fatalf("connecting to the test server: %v", err)
	}
	b := make([]byte, 6)
	_, _ = rand.Read(b)
	name := "hangar_test_" + hex.EncodeToString(b)
	if _, err := conn.Exec(ctx, `CREATE DATABASE `+pgx.Identifier{name}.Sanitize()); err != nil {
		conn.Close(ctx)
		t.Fatalf("creating a test database: %v", err)
	}
	t.Cleanup(func() {
		_, _ = conn.Exec(ctx, `DROP DATABASE `+pgx.Identifier{name}.Sanitize()+` WITH (FORCE)`)
		conn.Close(ctx)
	})

	u, err := url.Parse(admin)
	if err != nil {
		t.Fatalf("parsing HANGAR_TEST_DATABASE_URL: %v", err)
	}
	u.Path = "/" + name
	return u.String()
}

// Open returns a migrated database of the test's own.
func Open(t testing.TB) *db.DB {
	t.Helper()
	d, err := db.Open(context.Background(), URL(t))
	if err != nil {
		t.Fatalf("opening the test database: %v", err)
	}
	t.Cleanup(d.Close)
	return d
}
