package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"os"
	"os/user"
	"path/filepath"
	"strings"
	"time"

	"github.com/csnewman/hangar/internal/hangarsync"
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
	level := slog.LevelInfo
	if os.Getenv("HANGAR_AGENT_DEBUG") != "" {
		level = slog.LevelDebug
	}
	log := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: level}))
	g := profile.NewGuest(u, log)
	// Beside LockFS's backing, on the environment's own disk, so it
	// outlasts a reboot.
	if err := g.KeepStateIn("/var/lib/hangar/profile-state.json"); err != nil {
		log.Warn("profile: reading what it knew", "err", err)
	}
	// Before anything of the user's runs: a program must never take a lock
	// the server has not been asked about.
	if l, err := profile.MountLockFS(u, "/var/lib/hangar/lockfs", g); err != nil {
		log.Warn("profile: serving lock directories", "err", err)
	} else {
		g.SetBacking(l.Backing)
	}
	go serveKernelLocks(g, log)
	go func() {
		<-g.Synced()
		os.MkdirAll(filepath.Dir(sysuser.ProfileSynced), 0o755)
		if err := os.WriteFile(sysuser.ProfileSynced, nil, 0o644); err != nil {
			log.Warn("profile: marking it synced", "err", err)
		}
	}()
	go func() {
		for {
			if err := g.ServeSSHAgent(sysuser.SSHAuthSock); err != nil {
				fmt.Fprintf(os.Stderr, "hangar-agent: ssh agent: %v\n", err)
			}
			time.Sleep(5 * time.Second)
		}
	}()
	// Docker runs a credential helper by name from PATH: this agent,
	// under that name.
	helper := filepath.Join(filepath.Dir(agentInRoot), "docker-credential-"+profile.CredentialHelper)
	if err := os.Symlink(filepath.Base(agentInRoot), helper); err != nil && !os.IsExist(err) {
		log.Warn("profile: installing the docker credential helper", "err", err)
	}
	go func() {
		for {
			if err := g.ServeRegistryCredentials(sysuser.RegistrySock); err != nil {
				fmt.Fprintf(os.Stderr, "hangar-agent: registry credentials: %v\n", err)
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

// credentialHelper is Docker's credential helper protocol for Hangar's
// registry: get asks the agent for a credential for the host on stdin;
// store and erase have nothing to do, since Hangar gives the credentials;
// list names no stored ones. It returns the exit status.
func credentialHelper(args []string) int {
	if len(args) != 1 {
		fmt.Fprintln(os.Stderr, "usage: docker-credential-hangar get|store|erase|list")
		return 2
	}
	in, _ := io.ReadAll(os.Stdin)
	switch args[0] {
	case "get":
		host := strings.TrimSpace(string(in))
		host = strings.TrimPrefix(strings.TrimPrefix(host, "https://"), "http://")
		conn, err := net.Dial("unix", sysuser.RegistrySock)
		if err != nil {
			fmt.Println("credentials not found in native keychain")
			return 1
		}
		defer conn.Close()
		fmt.Fprintln(conn, host)
		var c profile.HelperCredential
		if err := json.NewDecoder(conn).Decode(&c); err != nil || c.Error != "" {
			if c.Error != "" {
				fmt.Fprintln(os.Stderr, "hangar:", c.Error)
			}
			// What docker takes as having no credentials, and pulls
			// anonymously.
			fmt.Println("credentials not found in native keychain")
			return 1
		}
		json.NewEncoder(os.Stdout).Encode(c)
		return 0
	case "store":
		fmt.Println("Hangar signs this environment in to its registry itself: no login is needed")
		return 1
	case "erase":
		return 0
	case "list":
		fmt.Println("{}")
		return 0
	}
	fmt.Fprintln(os.Stderr, "unknown action", args[0])
	return 2
}

// serveKernelLocks answers the kernel about locks on shared files, where it
// can ask (hangar-sync, in Hangar's kernel). A kernel without it keeps those
// locks local.
func serveKernelLocks(g *profile.Guest, log *slog.Logger) {
	for {
		dev, err := os.OpenFile(hangarsync.Device, os.O_RDWR, 0)
		if errors.Is(err, os.ErrNotExist) {
			return
		}
		if err != nil {
			log.Warn("profile: opening the kernel's lock device", "err", err)
			time.Sleep(5 * time.Second)
			continue
		}
		err = g.ServeLocks(dev)
		dev.Close()
		log.Warn("profile: serving the kernel's locks", "err", err)
		time.Sleep(time.Second)
	}
}
