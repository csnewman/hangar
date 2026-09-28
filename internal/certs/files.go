package certs

import (
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"log/slog"
	"os"
	"sync"
	"time"
)

// filesCheckEvery is the least time between two looks at whether the
// certificate files have changed.
const filesCheckEvery = 30 * time.Second

// Files serves a certificate and key someone else issues and renews, kept
// in PEM files: a wildcard certificate for the domain Hangar's hosts are
// named in, say. Replacing the files is picked up without a restart; a
// replacement that does not load leaves the last good one served.
type Files struct {
	certFile, keyFile string
	log               *slog.Logger

	mu      sync.Mutex
	cert    *tls.Certificate
	modTime [2]time.Time
	checked time.Time
}

// LoadFiles reads the certificate chain in certFile, leaf first, and its
// private key in keyFile.
func LoadFiles(certFile, keyFile string, log *slog.Logger) (*Files, error) {
	f := &Files{certFile: certFile, keyFile: keyFile, log: log}
	if err := f.load(); err != nil {
		return nil, err
	}
	return f, nil
}

func (f *Files) load() error {
	mods, err := f.modTimes()
	if err != nil {
		return err
	}
	c, err := tls.LoadX509KeyPair(f.certFile, f.keyFile)
	if err != nil {
		return fmt.Errorf("loading the certificate in %s and key in %s: %w", f.certFile, f.keyFile, err)
	}
	f.cert, f.modTime = &c, mods
	return nil
}

func (f *Files) modTimes() ([2]time.Time, error) {
	var t [2]time.Time
	for i, name := range []string{f.certFile, f.keyFile} {
		st, err := os.Stat(name)
		if err != nil {
			return t, err
		}
		t[i] = st.ModTime()
	}
	return t, nil
}

// Leaf is the certificate served, which says the names it covers.
func (f *Files) Leaf() *x509.Certificate {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.cert.Leaf
}

// TLSConfig is the configuration a TLS listener serves the certificate
// with.
func (f *Files) TLSConfig() *tls.Config {
	return &tls.Config{
		MinVersion:     tls.VersionTLS12,
		NextProtos:     []string{"h2", "http/1.1"},
		GetCertificate: func(*tls.ClientHelloInfo) (*tls.Certificate, error) { return f.current(), nil },
	}
}

// current is the certificate to serve, reloaded first if the files have
// changed since it was loaded.
func (f *Files) current() *tls.Certificate {
	f.mu.Lock()
	defer f.mu.Unlock()
	if time.Since(f.checked) < filesCheckEvery {
		return f.cert
	}
	f.checked = time.Now()
	mods, err := f.modTimes()
	if err != nil || mods == f.modTime {
		return f.cert
	}
	prev := f.cert
	if err := f.load(); err != nil {
		f.log.Warn("the certificate files changed but did not load; serving the last good certificate", "err", err)
		f.cert = prev
		// Tried again once they change again.
		f.modTime = mods
		return f.cert
	}
	f.log.Info("loaded the replaced certificate", "names", f.cert.Leaf.DNSNames, "expires", f.cert.Leaf.NotAfter)
	return f.cert
}
