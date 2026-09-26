// Package sshgw is Hangar's SSH gateway: one SSH server, on the control
// plane, in front of every environment.
//
//	ssh prof-a@hangar.example.com -p 2222        one of your environments
//	ssh alice/prof-a@hangar.example.com -p 2222  someone else's, if you may reach it
//
// SSH has nothing like HTTP's Host header, so the environment is named in
// the username. The person is known by their key: one of the sign-in keys
// on their Profile page. The gateway ends the connection's encryption to
// read both, and carries each channel the client opens -- shells, commands,
// port forwards -- to the SSH server in the environment's agent, over the
// worker's tunnel. Nothing reaches an environment from outside but through
// here, and every connection is in the audit log.
package sshgw

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"strings"
	"sync"
	"time"

	"github.com/jackc/pgx/v5"
	"golang.org/x/crypto/ssh"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/tunnel"
	"github.com/csnewman/hangar/internal/users"
)

// Tunnels opens streams to workers.
type Tunnels interface {
	Open(workerID string, h tunnel.Header) (net.Conn, error)
}

// Gateway serves SSH for every environment.
type Gateway struct {
	envs     *environments.Manager
	profiles *profile.Store
	tunnels  Tunnels
	audit    *audit.Log
	log      *slog.Logger
	config   *ssh.ServerConfig

	// KeepAlive is how often each client is asked for an answer, so that
	// an idle connection stays open through NATs and proxies that drop
	// quiet ones. A client that has not answered in three times this long
	// is gone, and its connection is closed.
	KeepAlive time.Duration
}

// New prepares a gateway presenting hostKey.
func New(hostKey ssh.Signer, envs *environments.Manager, profiles *profile.Store, tunnels Tunnels,
	log *audit.Log, logger *slog.Logger) *Gateway {
	g := &Gateway{envs: envs, profiles: profiles, tunnels: tunnels, audit: log, log: logger, KeepAlive: 30 * time.Second}
	g.config = &ssh.ServerConfig{
		ServerVersion:     "SSH-2.0-hangar",
		PublicKeyCallback: g.authenticate,
	}
	g.config.AddHostKey(hostKey)
	return g
}

// Serve accepts connections on addr until ctx ends.
func (g *Gateway) Serve(ctx context.Context, addr string) error {
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return err
	}
	g.log.Info("serving SSH", "addr", addr)
	go func() {
		<-ctx.Done()
		ln.Close()
	}()
	for {
		conn, err := ln.Accept()
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			return err
		}
		go g.serve(ctx, conn)
	}
}

// authenticate lets a key in if its owner may reach the environment the
// username names: "name" for one of their own, "owner/name" otherwise.
func (g *Gateway) authenticate(meta ssh.ConnMetadata, key ssh.PublicKey) (*ssh.Permissions, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	fp := ssh.FingerprintSHA256(key)
	owners, err := g.profiles.LoginKeyOwners(ctx, fp)
	if err != nil {
		g.log.Error("ssh: looking up a key", "err", err)
		return nil, errors.New("internal error")
	}
	ownerName, name, qualified := strings.Cut(meta.User(), "/")
	if !qualified {
		name = ownerName
	}
	for _, o := range owners {
		p := users.Principal{UserID: o.UserID, Username: o.Username, Admin: o.Admin}
		who := o.Username
		if qualified {
			who = ownerName
		}
		env, err := g.envs.ByName(ctx, p, who, name)
		if errors.Is(err, environments.ErrNotFound) || errors.Is(err, pgx.ErrNoRows) {
			continue
		}
		if err != nil {
			g.log.Error("ssh: looking up an environment", "err", err)
			return nil, errors.New("internal error")
		}
		return &ssh.Permissions{Extensions: map[string]string{
			"environment": env.ID, "user": o.UserID, "username": o.Username, "admin": fmt.Sprint(o.Admin), "key": fp,
		}}, nil
	}
	return nil, fmt.Errorf("no environment %q for this key", meta.User())
}

func (g *Gateway) serve(ctx context.Context, conn net.Conn) {
	defer conn.Close()
	conn.SetDeadline(time.Now().Add(30 * time.Second))
	down, chans, reqs, err := ssh.NewServerConn(conn, g.config)
	if err != nil {
		return
	}
	defer down.Close()
	conn.SetDeadline(time.Time{})
	done := make(chan struct{})
	defer close(done)
	go keepAlive(down, g.KeepAlive, done)
	ext := down.Permissions.Extensions
	ip, _, _ := net.SplitHostPort(conn.RemoteAddr().String())
	actx := audit.WithActor(ctx, audit.Actor{UserID: ext["user"], Name: ext["username"], IP: ip, Via: "ssh key " + ext["key"]})
	p := users.Principal{UserID: ext["user"], Username: ext["username"], Admin: ext["admin"] == "true"}
	env, err := g.envs.Get(actx, p, ext["environment"])
	if err != nil {
		refuse(chans, reqs, "the environment is gone")
		return
	}

	up, err := g.dial(env)
	if err != nil {
		g.access(actx, env, "environment.ssh_refused", map[string]any{"reason": err.Error()})
		refuse(chans, reqs, err.Error())
		return
	}
	defer up.Close()
	start := time.Now()
	g.access(actx, env, "environment.ssh_connect", map[string]any{"key": ext["key"], "client": string(down.ClientVersion())})
	defer func() {
		g.access(actx, env, "environment.ssh_disconnect", map[string]any{"seconds": int(time.Since(start).Seconds())})
	}()

	// Keepalives and the like go on to the guest, and its answers back.
	go forwardRequests(reqs, up)
	for nc := range chans {
		go proxyChannel(nc, up)
	}
}

// keepAlive asks the client for an answer every interval until done, and
// closes the connection when one has not come in three intervals.
func keepAlive(c ssh.Conn, interval time.Duration, done <-chan struct{}) {
	for {
		select {
		case <-done:
			return
		case <-time.After(interval):
		}
		answered := make(chan error, 1)
		go func() {
			_, _, err := c.SendRequest("keepalive@openssh.com", true, nil)
			answered <- err
		}()
		select {
		case <-done:
			return
		case err := <-answered:
			if err != nil {
				return
			}
		case <-time.After(3 * interval):
			c.Close()
			return
		}
	}
}

// dial connects to the environment's SSH server, through its worker.
func (g *Gateway) dial(env api.Environment) (ssh.Conn, error) {
	if env.Phase != api.PhaseRunning || env.WorkerID == "" {
		return nil, fmt.Errorf("%s is %s, not running: start it first", env.Name, env.Phase)
	}
	stream, err := g.tunnels.Open(env.WorkerID, tunnel.Header{Kind: tunnel.KindSSH, Environment: env.ID})
	if err != nil {
		return nil, errors.New("the environment's worker cannot be reached from this server just now")
	}
	// The guest's server is reached only through the worker's tunnel, which
	// is the server's own: its host key has nothing to prove.
	c, chans, reqs, err := ssh.NewClientConn(stream, env.ID, &ssh.ClientConfig{
		User: "dev", HostKeyCallback: ssh.InsecureIgnoreHostKey(), Timeout: 20 * time.Second,
	})
	if err != nil {
		stream.Close()
		return nil, fmt.Errorf("the environment's SSH server did not answer: %v", err)
	}
	// It opens no channels of its own.
	go func() {
		for nc := range chans {
			nc.Reject(ssh.Prohibited, "not carried")
		}
	}()
	go ssh.DiscardRequests(reqs)
	return c, nil
}

func (g *Gateway) access(ctx context.Context, env api.Environment, action string, details map[string]any) {
	if g.audit == nil {
		return
	}
	if err := g.audit.Record(ctx, audit.Event{Action: action, Target: environments.Ref(env.ID, env.Name),
		Related: []audit.Ref{{Type: audit.KindOwner, ID: env.OwnerID}, {Type: audit.KindWorker, ID: env.WorkerID}},
		Details: details}); err != nil {
		g.log.Error("ssh: recording", "action", action, "err", err)
	}
}

// refuse answers every session the client opens with why it cannot have
// one, and ends it.
func refuse(chans <-chan ssh.NewChannel, reqs <-chan *ssh.Request, why string) {
	go ssh.DiscardRequests(reqs)
	for nc := range chans {
		if nc.ChannelType() != "session" {
			nc.Reject(ssh.ConnectionFailed, why)
			continue
		}
		ch, creqs, err := nc.Accept()
		if err != nil {
			continue
		}
		go func() {
			for r := range creqs {
				if r.WantReply {
					r.Reply(r.Type == "shell" || r.Type == "exec" || r.Type == "pty-req" || r.Type == "env", nil)
				}
				if r.Type == "shell" || r.Type == "exec" {
					fmt.Fprintf(ch.Stderr(), "hangar: %s\r\n", why)
					ch.SendRequest("exit-status", false, []byte{0, 0, 0, 1})
					ch.Close()
				}
			}
		}()
	}
}

// forwardRequests carries global requests one way, and their answers back.
func forwardRequests(reqs <-chan *ssh.Request, to ssh.Conn) {
	for r := range reqs {
		ok, payload, err := to.SendRequest(r.Type, r.WantReply, r.Payload)
		if r.WantReply {
			r.Reply(ok && err == nil, payload)
		}
	}
}

// proxyChannel opens the same channel on the guest's side and carries
// everything between the two: data both ways, the error stream, requests
// and their answers, and each side's end.
func proxyChannel(nc ssh.NewChannel, up ssh.Conn) {
	uch, ureqs, err := up.OpenChannel(nc.ChannelType(), nc.ExtraData())
	if err != nil {
		var open *ssh.OpenChannelError
		if errors.As(err, &open) {
			nc.Reject(open.Reason, open.Message)
		} else {
			nc.Reject(ssh.ConnectionFailed, err.Error())
		}
		return
	}
	dch, dreqs, err := nc.Accept()
	if err != nil {
		uch.Close()
		return
	}
	// The client's input goes on until it ends it; the guest's output, and
	// its error stream, until the guest ends them.
	go func() { io.Copy(uch, dch); uch.CloseWrite() }()
	var out sync.WaitGroup
	out.Add(2)
	go func() { io.Copy(dch, uch); out.Done() }()
	go func() { io.Copy(dch.Stderr(), uch.Stderr()); out.Done() }()
	outDone := make(chan struct{})
	go func() {
		out.Wait()
		dch.CloseWrite()
		close(outDone)
	}()
	go channelRequests(dreqs, uch)
	// The guest's requests -- exit-status, above all -- are sent once its
	// output has all been carried, so the client has it before it hears
	// the command is done; and once the guest closes the channel, so does
	// the client's.
	go func() {
		for r := range ureqs {
			if r.Type == "exit-status" || r.Type == "exit-signal" || r.Type == "eow@openssh.com" {
				select {
				case <-outDone:
				case <-time.After(5 * time.Second):
				}
			}
			ok, _ := dch.SendRequest(r.Type, r.WantReply, r.Payload)
			if r.WantReply {
				r.Reply(ok, nil)
			}
		}
		select {
		case <-outDone:
		case <-time.After(5 * time.Second):
		}
		dch.Close()
	}()
}

// channelRequests carries a channel's requests to the other side, and the
// answers back.
func channelRequests(reqs <-chan *ssh.Request, to ssh.Channel) {
	for r := range reqs {
		ok, err := to.SendRequest(r.Type, r.WantReply, r.Payload)
		if r.WantReply {
			r.Reply(ok && err == nil, nil)
		}
	}
	to.Close()
}

// HostKey is the gateway's host key: made once and kept in the database,
// sealed with the server's secret key, so every replica presents the same
// one and clients are not warned it changed. Without a secret key it is
// kept unsealed.
func HostKey(ctx context.Context, d *db.DB, sealer *profile.Sealer) (ssh.Signer, error) {
	const name = "ssh-gateway-host-key"
	_, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		return nil, err
	}
	block, err := ssh.MarshalPrivateKey(priv, "hangar")
	if err != nil {
		return nil, err
	}
	fresh := pem.EncodeToMemory(block)
	value, sealed := fresh, false
	if sealer != nil {
		if value, err = sealer.Seal(fresh, name); err != nil {
			return nil, err
		}
		sealed = true
	}
	var stored []byte
	var storedSealed bool
	err = d.Transact(ctx, func(tx db.Tx) error {
		// Whichever replica gets there first makes it; the rest read it.
		if _, err := tx.Exec(ctx, `INSERT INTO server_keys (name, value, sealed) VALUES ($1, $2, $3)
			ON CONFLICT (name) DO NOTHING`, name, value, sealed); err != nil {
			return err
		}
		return tx.QueryRow(ctx, `SELECT value, sealed FROM server_keys WHERE name = $1`, name).Scan(&stored, &storedSealed)
	})
	if err != nil {
		return nil, err
	}
	if storedSealed {
		if sealer == nil {
			return nil, errors.New("the SSH gateway's host key is sealed, and the server has no secret key to open it")
		}
		if stored, err = sealer.Open(stored, name); err != nil {
			return nil, err
		}
	}
	key, err := ssh.ParseRawPrivateKey(stored)
	if err != nil {
		return nil, err
	}
	return ssh.NewSignerFromKey(key)
}
