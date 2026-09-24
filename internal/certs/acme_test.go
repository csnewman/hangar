package certs_test

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"log/slog"
	"net"
	"net/netip"
	"os"
	"slices"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/certs"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/zone"
)

// TestIssue is issued the zone's certificates, one of them a wildcard, by a real ACME CA, answering
// its DNS-01 challenge from the zone. It runs against Pebble, Let's
// Encrypt's test CA, when these are set:
//
//	HANGAR_TEST_ACME_DIRECTORY   Pebble's directory, https://localhost:14000/dir
//	HANGAR_TEST_ACME_ROOTS       the PEM root Pebble's own endpoint is served under
//	HANGAR_TEST_ACME_DNS_LISTEN  where to serve the zone: Pebble's -dnsserver
func TestIssue(t *testing.T) {
	dir := os.Getenv("HANGAR_TEST_ACME_DIRECTORY")
	if dir == "" {
		t.Skip("HANGAR_TEST_ACME_DIRECTORY is not set")
	}
	roots, err := os.ReadFile(os.Getenv("HANGAR_TEST_ACME_ROOTS"))
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	log := slog.New(slog.NewTextHandler(t.Output(), nil))

	d := dbtest.Open(t)
	records := zone.NewManager(d)
	dns, err := zone.NewServer(zone.Config{Zone: "hangar.test", Addresses: []netip.Addr{netip.MustParseAddr("127.0.0.1")}}, records, log)
	if err != nil {
		t.Fatal(err)
	}
	go dns.Serve(ctx, os.Getenv("HANGAR_TEST_ACME_DNS_LISTEN"))

	m, err := certs.New(d, records, certs.Options{Zone: "hangar.test", Email: "admin@hangar.test", CA: dir, CARoots: roots}, log)
	if err != nil {
		t.Fatal(err)
	}
	if err := m.Start(ctx); err != nil {
		t.Fatal(err)
	}

	// The zone's own name has a certificate, and every name under it is
	// served the wildcard one.
	names := func(c *tls.Certificate) []string {
		leaf, err := x509.ParseCertificate(c.Certificate[0])
		if err != nil {
			t.Fatal(err)
		}
		return leaf.DNSNames
	}
	apex := waitCert(ctx, t, m, "hangar.test")
	if n := names(apex); !slices.Equal(n, []string{"hangar.test"}) {
		t.Errorf("the zone's certificate names %v", n)
	}
	wildcard := waitCert(ctx, t, m, "e-2a04a86a-260e-4fdf-9d94-d1b5a1ce8a7c.hangar.test")
	if n := names(wildcard); !slices.Equal(n, []string{"*.hangar.test"}) {
		t.Errorf("an editor's certificate names %v", n)
	}

	// The challenge's records are gone once it is done.
	if recs, err := records.Lookup(ctx, "_acme-challenge.hangar.test", "TXT"); err != nil || len(recs) != 0 {
		t.Errorf("challenge records left: %v %v", recs, err)
	}

	// Another replica, sharing only the database, serves the same
	// certificate without being issued its own.
	other, err := certs.New(d, records, certs.Options{Zone: "hangar.test", CA: dir, CARoots: roots}, log)
	if err != nil {
		t.Fatal(err)
	}
	if err := other.Start(ctx); err != nil {
		t.Fatal(err)
	}
	if c := waitCert(ctx, t, other, "hangar.test"); !slices.Equal(c.Certificate[0], apex.Certificate[0]) {
		t.Error("a second replica was issued its own certificate")
	}
}

func waitCert(ctx context.Context, t *testing.T, m *certs.Manager, name string) *tls.Certificate {
	t.Helper()
	// certmagic logs the connection a hello came on.
	conn, peer := net.Pipe()
	defer conn.Close()
	defer peer.Close()
	for {
		c, err := m.TLSConfig().GetCertificate(&tls.ClientHelloInfo{ServerName: name, Conn: conn})
		if err == nil && c != nil {
			return c
		}
		select {
		case <-ctx.Done():
			t.Fatalf("no certificate for %s: %v", name, err)
		case <-time.After(500 * time.Millisecond):
		}
	}
}
