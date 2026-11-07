package worker

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"math/big"
	"net"
	"net/http"
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/guestport"
)

// DialPorts connects to a running environment's port forwarder. With no
// guest, a web server that reports what reached it listens on ports 80 and
// 8000, and on 443 over TLS with a certificate of its own; nothing listens
// on any other port.
func (s *Simulated) DialPorts(_ context.Context, id string) (net.Conn, error) {
	s.mu.Lock()
	e, ok := s.envs[id]
	running := ok && e.phase == api.PhaseRunning
	s.mu.Unlock()
	if !running {
		return nil, fmt.Errorf("environment %s is not running here", id)
	}
	a, b := net.Pipe()
	go func() {
		var req [2]byte
		if _, err := io.ReadFull(b, req[:]); err != nil {
			b.Close()
			return
		}
		port := binary.BigEndian.Uint16(req[:])
		srv := &http.Server{Handler: simulatedPort(id, int(port))}
		switch port {
		case 80, 8000:
		case 443:
			cert, err := simulatedCert()
			if err != nil {
				b.Write([]byte{guestport.Failed})
				b.Close()
				return
			}
			srv.TLSConfig = &tls.Config{Certificates: []tls.Certificate{cert}}
		default:
			b.Write([]byte{guestport.Refused})
			b.Close()
			return
		}
		if _, err := b.Write([]byte{guestport.Connected}); err != nil {
			b.Close()
			return
		}
		ln := &oneConn{conn: b, done: make(chan struct{})}
		if srv.TLSConfig != nil {
			srv.ServeTLS(ln, "", "")
			return
		}
		srv.Serve(ln)
	}()
	return a, nil
}

// SimulatedRequest is what a simulated environment's web server reports
// about a request.
type SimulatedRequest struct {
	Environment string      `json:"environment"`
	Host        string      `json:"host"`
	Path        string      `json:"path"`
	Header      http.Header `json:"header"`
}

// SimulatedPortRequest is what the simulated environment's web server
// reports about a request.
type SimulatedPortRequest struct {
	SimulatedRequest
	Port int  `json:"port"`
	TLS  bool `json:"tls"`
}

func simulatedPort(id string, port int) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(SimulatedPortRequest{
			SimulatedRequest: SimulatedRequest{Environment: id, Host: r.Host, Path: r.URL.RequestURI(), Header: r.Header},
			Port:             port,
			TLS:              r.TLS != nil,
		})
	})
}

var (
	simulatedCertOnce sync.Once
	simulatedCertPair tls.Certificate
	simulatedCertErr  error
)

// simulatedCert is a self-signed certificate, as a development server in an
// environment would have.
func simulatedCert() (tls.Certificate, error) {
	simulatedCertOnce.Do(func() {
		key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
		if err != nil {
			simulatedCertErr = err
			return
		}
		tmpl := &x509.Certificate{SerialNumber: big.NewInt(1), Subject: pkix.Name{CommonName: "localhost"},
			DNSNames: []string{"localhost"}, NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(24 * time.Hour)}
		der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
		if err != nil {
			simulatedCertErr = err
			return
		}
		simulatedCertPair = tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}
	})
	return simulatedCertPair, simulatedCertErr
}

// oneConn is a listener that yields one connection, and then nothing until
// that connection closes.
type oneConn struct {
	conn net.Conn
	once sync.Once
	done chan struct{}
}

func (l *oneConn) Accept() (net.Conn, error) {
	var c net.Conn
	l.once.Do(func() { c = &closeNotify{Conn: l.conn, done: l.done} })
	if c != nil {
		return c, nil
	}
	<-l.done
	return nil, net.ErrClosed
}

func (l *oneConn) Close() error   { return nil }
func (l *oneConn) Addr() net.Addr { return l.conn.LocalAddr() }

type closeNotify struct {
	net.Conn
	once sync.Once
	done chan struct{}
}

func (c *closeNotify) Close() error {
	c.once.Do(func() { close(c.done) })
	return c.Conn.Close()
}
