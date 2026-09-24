package main

import (
	"fmt"
	"log/slog"
	"os"
	"os/user"
	"path/filepath"
	"time"

	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/sysuser"
	"github.com/csnewman/hangar/internal/vsock"
)

// serveProfile keeps the user's home directory in step with their profile,
// and serves their SSH agent, for as long as the guest runs.
func serveProfile() {
	sshSetup()
	u, err := user.Lookup("dev")
	for err != nil {
		// The image has no such user, or not yet: users can be made by
		// units that run after the agent starts.
		time.Sleep(10 * time.Second)
		u, err = user.Lookup("dev")
	}
	log := slog.New(slog.NewTextHandler(os.Stderr, nil))
	g := profile.NewGuest(u, log)
	// Before anything of the user's runs: a program must never take a lock
	// the server has not been asked about.
	if l, err := profile.MountLockFS(u, "/var/lib/hangar/lockfs", g); err != nil {
		log.Warn("profile: serving lock directories", "err", err)
	} else {
		g.SetBacking(l.Backing)
	}
	go func() {
		for {
			if err := g.ServeSSHAgent(sysuser.SSHAuthSock); err != nil {
				fmt.Fprintf(os.Stderr, "hangar-agent: ssh agent: %v\n", err)
			}
			time.Sleep(5 * time.Second)
		}
	}()
	for {
		ln, err := vsock.Listen(vsock.CIDAny, profile.Port)
		if err != nil {
			fmt.Fprintf(os.Stderr, "hangar-agent: profile: %v\n", err)
			time.Sleep(5 * time.Second)
			continue
		}
		for {
			conn, _, err := ln.Accept(0)
			if err != nil {
				fmt.Fprintf(os.Stderr, "hangar-agent: profile: %v\n", err)
				break
			}
			go g.Serve(conn)
		}
		ln.Close()
	}
}

// sshSetup points every login at the SSH agent, and has ssh accept a host
// it has not met: an environment is new, so every host is one it has not
// met, and a prompt in the middle of a clone is no protection.
func sshSetup() {
	files := map[string]string{
		"/etc/profile.d/hangar-ssh.sh":         "export SSH_AUTH_SOCK=" + sysuser.SSHAuthSock + "\n",
		"/etc/ssh/ssh_config.d/50-hangar.conf": "Host *\n    StrictHostKeyChecking accept-new\n",
	}
	for path, content := range files {
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err == nil {
			os.WriteFile(path, []byte(content), 0o644)
		}
	}
}
