package certs_test

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"log/slog"
	"math/big"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/certs"
)

// writePair writes a self-signed certificate for names and its key, and
// returns their paths.
func writePair(t *testing.T, dir, prefix string, names ...string) (string, string) {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	tmpl := &x509.Certificate{SerialNumber: big.NewInt(time.Now().UnixNano()), Subject: pkix.Name{CommonName: names[0]},
		DNSNames: names, NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(time.Hour)}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	if err != nil {
		t.Fatal(err)
	}
	keyDER, err := x509.MarshalECPrivateKey(key)
	if err != nil {
		t.Fatal(err)
	}
	certFile, keyFile := filepath.Join(dir, prefix+"cert.pem"), filepath.Join(dir, prefix+"key.pem")
	if err := os.WriteFile(certFile, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der}), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(keyFile, pem.EncodeToMemory(&pem.Block{Type: "EC PRIVATE KEY", Bytes: keyDER}), 0o600); err != nil {
		t.Fatal(err)
	}
	return certFile, keyFile
}

func TestFiles(t *testing.T) {
	dir := t.TempDir()
	certFile, keyFile := writePair(t, dir, "", "*.example.com", "example.com")
	f, err := certs.LoadFiles(certFile, keyFile, slog.Default())
	if err != nil {
		t.Fatal(err)
	}
	if err := f.Leaf().VerifyHostname("e-2a04a86a-6d8e-4f0e-9c1b-0d3c1a2b3c4d-hangar1.example.com"); err != nil {
		t.Errorf("a prefix name under the wildcard: %v", err)
	}
	served, err := f.TLSConfig().GetCertificate(&tls.ClientHelloInfo{ServerName: "hangar1.example.com"})
	if err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(served.Leaf.DNSNames, []string{"*.example.com", "example.com"}) {
		t.Errorf("served %v", served.Leaf.DNSNames)
	}

	// A key that is not the certificate's, or a file that is not there,
	// refuses to start rather than serve nothing.
	_, otherKey := writePair(t, dir, "other-", "other.example.com")
	if _, err := certs.LoadFiles(certFile, otherKey, slog.Default()); err == nil {
		t.Error("loaded a certificate with another's key")
	}
	if _, err := certs.LoadFiles(filepath.Join(dir, "missing.pem"), keyFile, slog.Default()); err == nil {
		t.Error("loaded a missing certificate")
	}
}
