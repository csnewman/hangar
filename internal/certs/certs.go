package certs

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"log/slog"
	"time"

	"github.com/caddyserver/certmagic"
	"github.com/mholt/acmez/v3/acme"
	"go.uber.org/zap"

	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/zone"
)

// Options configure certificate management.
type Options struct {
	// Zone is Hangar's host. It is issued certificates for it and for *.Zone.
	Zone string
	// Email is the ACME account's contact, where the CA sends expiry
	// warnings.
	Email string
	// CA is the ACME directory. Empty is Let's Encrypt.
	CA string
	// CARoots, if set, are the PEM roots to trust for the CA itself: for a
	// test CA such as Pebble.
	CARoots []byte
}

// Manager obtains and renews the certificates and serves them.
type Manager struct {
	cfg     *certmagic.Config
	domains []string
	log     *slog.Logger
}

// New prepares certificate management. Start begins obtaining.
func New(d *db.DB, records *zone.Manager, o Options, log *slog.Logger) (*Manager, error) {
	domain := zone.Canonical(o.Zone)
	if domain == "" {
		return nil, fmt.Errorf("no zone to be issued a certificate for")
	}
	storage := NewStorage(d)
	logger := zap.New(&slogCore{log: log.With("component", "acme")})
	var cache *certmagic.Cache
	cache = certmagic.NewCache(certmagic.CacheOptions{
		GetConfigForCert: func(certmagic.Certificate) (*certmagic.Config, error) {
			return certmagic.New(cache, certmagic.Config{Storage: storage, Logger: logger}), nil
		},
		Logger: logger,
	})
	cfg := certmagic.New(cache, certmagic.Config{Storage: storage, Logger: logger})

	template := certmagic.ACMEIssuer{
		CA:                      certmagic.LetsEncryptProductionCA,
		Email:                   o.Email,
		Agreed:                  true,
		DisableHTTPChallenge:    true,
		DisableTLSALPNChallenge: true,
		DNS01Solver:             &solver{records: records},
		Logger:                  logger,
	}
	if o.CA != "" {
		template.CA = o.CA
	}
	if len(o.CARoots) > 0 {
		pool, err := rootPool(o.CARoots)
		if err != nil {
			return nil, err
		}
		template.TrustedRoots = pool
	}
	issuer := certmagic.NewACMEIssuer(cfg, template)
	cfg.Issuers = []certmagic.Issuer{issuer}
	return &Manager{cfg: cfg, domains: []string{domain, "*." + domain}, log: log}, nil
}

// Start obtains any certificate there is none of yet, and keeps them
// renewed, in the background. Until a name has one, TLS handshakes for it
// fail.
func (m *Manager) Start(ctx context.Context) error {
	m.log.Info("managing TLS certificates", "names", m.domains)
	return m.cfg.ManageAsync(ctx, m.domains)
}

// TLSConfig is the configuration a TLS listener serves the certificates
// with.
func (m *Manager) TLSConfig() *tls.Config {
	t := m.cfg.TLSConfig()
	// ACME's TLS-ALPN challenge is off, so only HTTP is negotiated.
	t.NextProtos = []string{"h2", "http/1.1"}
	return t
}

// solver answers the DNS-01 challenge by adding its TXT record to the
// zone. Every replica serves the zone from the table the record is written
// to, so it is served the moment it is written: there is no propagation to
// wait for.
type solver struct {
	records *zone.Manager
}

func (s *solver) record(c acme.Challenge) zone.Record {
	return zone.Record{Name: c.DNS01TXTRecordName(), Type: "TXT", Value: c.DNS01KeyAuthorization(), TTL: time.Minute}
}

func (s *solver) Present(ctx context.Context, c acme.Challenge) error {
	return s.records.Add(ctx, s.record(c))
}

func (s *solver) CleanUp(ctx context.Context, c acme.Challenge) error {
	return s.records.Remove(ctx, s.record(c))
}

// rootPool is a certificate pool of PEM roots.
func rootPool(pem []byte) (*x509.CertPool, error) {
	pool := x509.NewCertPool()
	if !pool.AppendCertsFromPEM(pem) {
		return nil, fmt.Errorf("no certificates in the CA roots given")
	}
	return pool, nil
}
