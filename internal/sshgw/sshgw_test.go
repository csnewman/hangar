package sshgw_test

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"errors"
	"io"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/pkg/sftp"
	"golang.org/x/crypto/ssh"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/sshd"
	"github.com/csnewman/hangar/internal/sshgw"
	"github.com/csnewman/hangar/internal/tunnel"
)

// TestMain lets the test binary be the guest's sftp-server.
func TestMain(m *testing.M) {
	if os.Getenv("HANGAR_TEST_SFTP") == "1" {
		if err := sshd.ServeSFTP(); err != nil {
			os.Exit(1)
		}
		return
	}
	os.Exit(m.Run())
}

// guests stands in for workers: an environment's SSH stream is a pipe to
// the guest's SSH server, run here.
type guests struct{ worker string }

func (g guests) Open(worker string, h tunnel.Header) (net.Conn, error) {
	if worker != g.worker || h.Kind != tunnel.KindSSH {
		return nil, errors.New("no such worker")
	}
	srv, err := sshd.NewServer("nobody-here", slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		return nil, err
	}
	a, b, err := sshd.Pipe()
	if err != nil {
		return nil, err
	}
	if exe, err := os.Executable(); err == nil {
		srv.SFTP = "HANGAR_TEST_SFTP=1 " + exe
	}
	go srv.Serve(b)
	return a, nil
}

func newKey(t *testing.T) (ssh.Signer, string) {
	_, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	s, err := ssh.NewSignerFromKey(priv)
	if err != nil {
		t.Fatal(err)
	}
	return s, string(ssh.MarshalAuthorizedKey(s.PublicKey()))
}

func TestGateway(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	d := dbtest.Open(t)
	store := profile.NewStore(d, nil)

	var alice, bob, worker, running, stopped string
	err := d.Transact(ctx, func(tx db.Tx) error {
		tx.QueryRow(ctx, `INSERT INTO users (username) VALUES ('alice') RETURNING id`).Scan(&alice)
		tx.QueryRow(ctx, `INSERT INTO users (username) VALUES ('bob') RETURNING id`).Scan(&bob)
		tx.QueryRow(ctx, `INSERT INTO workers (name, credential_hash) VALUES ('w', '\x00') RETURNING id`).Scan(&worker)
		for _, e := range []struct {
			name, phase string
			id          *string
		}{{"dev", "running", &running}, {"idle", "stopped", &stopped}} {
			if err := tx.QueryRow(ctx, `INSERT INTO environments (owner_id, name, template_name, spec, image, cpus,
				memory_mib, desired, worker_id, phase)
				VALUES ($1, $2, 't', '{}', 'img', 1, 512, 'running', $3, $4) RETURNING id`,
				alice, e.name, worker, e.phase).Scan(e.id); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	aliceKey, alicePub := newKey(t)
	bobKey, bobPub := newKey(t)
	if _, err := store.AddLoginKey(ctx, alice, "laptop", alicePub); err != nil {
		t.Fatal(err)
	}
	if _, err := store.AddLoginKey(ctx, bob, "laptop", bobPub); err != nil {
		t.Fatal(err)
	}

	host, err := sshgw.HostKey(ctx, d, nil)
	if err != nil {
		t.Fatal(err)
	}
	again, err := sshgw.HostKey(ctx, d, nil)
	if err != nil || !bytes.Equal(again.PublicKey().Marshal(), host.PublicKey().Marshal()) {
		t.Fatal("a second replica would present a different host key")
	}
	gw := sshgw.New(host, environments.NewManager(d), store, guests{worker}, audit.NewLog(d),
		slog.New(slog.NewTextHandler(io.Discard, nil)))
	gw.KeepAlive = 50 * time.Millisecond
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	addr := ln.Addr().String()
	ln.Close()
	go gw.Serve(ctx, addr)

	dial := func(user string, key ssh.Signer) (*ssh.Client, error) {
		cfg := &ssh.ClientConfig{User: user, Auth: []ssh.AuthMethod{ssh.PublicKeys(key)},
			HostKeyCallback: ssh.FixedHostKey(host.PublicKey()), Timeout: 5 * time.Second}
		var c *ssh.Client
		var err error
		for range 50 {
			if c, err = ssh.Dial("tcp", addr, cfg); err == nil || !strings.Contains(err.Error(), "refused") {
				break
			}
			time.Sleep(20 * time.Millisecond)
		}
		return c, err
	}

	c, err := dial("dev", aliceKey)
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()

	// A command, its output and its exit code.
	s, _ := c.NewSession()
	var out, errOut bytes.Buffer
	s.Stdout, s.Stderr = &out, &errOut
	err = s.Run("echo hello; echo oops >&2; exit 3")
	var exit *ssh.ExitError
	if !errors.As(err, &exit) || exit.ExitStatus() != 3 || out.String() != "hello\n" || errOut.String() != "oops\n" {
		t.Fatalf("a command: %v, %q, %q", err, out.String(), errOut.String())
	}

	// A terminal.
	s, _ = c.NewSession()
	if err := s.RequestPty("xterm", 24, 80, ssh.TerminalModes{}); err != nil {
		t.Fatal(err)
	}
	b, err := s.CombinedOutput("tty")
	if err != nil || !strings.HasPrefix(string(b), "/dev/") {
		t.Fatalf("a terminal: %q, %v", b, err)
	}

	// A port forwarded from here to something listening in the
	// environment -- this machine, in this test.
	svc, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer svc.Close()
	go func() {
		conn, err := svc.Accept()
		if err == nil {
			conn.Write([]byte("from inside"))
			conn.Close()
		}
	}()
	fwd, err := c.Dial("tcp", svc.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	got, _ := io.ReadAll(fwd)
	if string(got) != "from inside" {
		t.Fatalf("a forwarded port gave %q", got)
	}

	// Files, over SFTP.
	sc, err := sftp.NewClient(c)
	if err != nil {
		t.Fatal(err)
	}
	name := filepath.Join(t.TempDir(), "note")
	f, err := sc.Create(name)
	if err != nil {
		t.Fatal(err)
	}
	f.Write([]byte("over sftp"))
	f.Close()
	if b, err := os.ReadFile(name); err != nil || string(b) != "over sftp" {
		t.Fatalf("a file written over SFTP: %q, %v", b, err)
	}
	sc.Close()

	// Bob's key is not alice's, and alice/dev is not bob's to reach.
	if _, err := dial("dev", bobKey); err == nil {
		t.Error("bob's key reached alice's environment")
	}
	if _, err := dial("alice/dev", bobKey); err == nil {
		t.Error("bob reached alice/dev")
	}
	if _, err := dial("nothing", aliceKey); err == nil {
		t.Error("a name with no environment was let in")
	}

	// A stopped environment says so.
	c2, err := dial("idle", aliceKey)
	if err != nil {
		t.Fatal(err)
	}
	defer c2.Close()
	s, _ = c2.NewSession()
	errOut.Reset()
	s.Stderr = &errOut
	if err := s.Run("true"); err == nil || !strings.Contains(errOut.String(), "not running") {
		t.Errorf("a stopped environment: %v, %q", err, errOut.String())
	}

	// A client that answers keepalives stays connected however long it
	// is quiet; one that does not is let go.
	time.Sleep(10 * gw.KeepAlive)
	s, _ = c.NewSession()
	if b, err := s.Output("echo still here"); err != nil || string(b) != "still here\n" {
		t.Fatalf("a quiet client that answers keepalives: %q, %v", b, err)
	}
	raw, err := net.Dial("tcp", addr)
	if err != nil {
		t.Fatal(err)
	}
	silent, _, reqs, err := ssh.NewClientConn(raw, addr, &ssh.ClientConfig{User: "dev",
		Auth: []ssh.AuthMethod{ssh.PublicKeys(aliceKey)}, HostKeyCallback: ssh.FixedHostKey(host.PublicKey())})
	if err != nil {
		t.Fatal(err)
	}
	var asked atomic.Int32
	go func() {
		for range reqs {
			asked.Add(1)
		}
	}()
	gone := make(chan error, 1)
	go func() { gone <- silent.Wait() }()
	select {
	case <-gone:
		if asked.Load() == 0 {
			t.Error("a client was let go without being asked for an answer")
		}
	case <-time.After(20 * gw.KeepAlive):
		t.Error("a client that never answers keepalives is still connected")
	}

	entries, _ := audit.NewLog(d).List(ctx, audit.Query{Subjects: []string{"environment:" + running}})
	var actions []string
	for _, e := range entries {
		actions = append(actions, e.Action+" by "+e.ActorName)
	}
	if len(actions) == 0 || !strings.Contains(strings.Join(actions, ","), "environment.ssh_connect by alice") {
		t.Errorf("the connection was not recorded: %v", actions)
	}
}
