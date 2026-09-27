package profile

import (
	"bufio"
	"encoding/json"
	"errors"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

// CredentialHelper is the name Docker runs a credential helper by,
// docker-credential-<name>, and the one the Docker configuration names for
// Hangar's registry.
const CredentialHelper = "hangar"

// setRegistry has Docker use the credential helper for Hangar's registry,
// for the user and for root, so docker push and sudo docker push both
// reach it as the environment's owner.
func (g *Guest) setRegistry(host string) {
	uid, _ := strconv.Atoi(g.user.Uid)
	gid, _ := strconv.Atoi(g.user.Gid)
	for _, c := range []struct {
		home     string
		uid, gid int
	}{{g.user.HomeDir, uid, gid}, {"/root", 0, 0}} {
		if err := useHelper(filepath.Join(c.home, ".docker", "config.json"), host, c.uid, c.gid); err != nil {
			g.log.Warn("profile: configuring docker for Hangar's registry", "home", c.home, "err", err)
		}
	}
}

// useHelper names the credential helper for host in a Docker configuration,
// keeping the rest of it as it is.
func useHelper(path, host string, uid, gid int) error {
	cfg := map[string]any{}
	b, err := os.ReadFile(path)
	switch {
	case err == nil:
		if err := json.Unmarshal(b, &cfg); err != nil {
			return err
		}
	case !errors.Is(err, os.ErrNotExist):
		return err
	}
	helpers, _ := cfg["credHelpers"].(map[string]any)
	if helpers == nil {
		helpers = map[string]any{}
	}
	if helpers[host] == CredentialHelper {
		return nil
	}
	helpers[host] = CredentialHelper
	cfg["credHelpers"] = helpers
	out, err := json.MarshalIndent(cfg, "", "\t")
	if err != nil {
		return err
	}
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	if err := os.Chown(dir, uid, gid); err != nil {
		return err
	}
	tmp := path + ".hangar"
	if err := os.WriteFile(tmp, append(out, '\n'), 0o600); err != nil {
		return err
	}
	if err := os.Chown(tmp, uid, gid); err != nil {
		os.Remove(tmp)
		return err
	}
	return os.Rename(tmp, path)
}

// credentialWait is how long the credential helper waits for a session to
// ask through.
const credentialWait = 30 * time.Second

// ServeRegistryCredentials serves, on socket, credentials for Hangar's
// registry to the credential helper: it writes the registry's host, and is
// answered with the Docker helper's JSON for it, or an error.
func (g *Guest) ServeRegistryCredentials(socket string) error {
	if err := os.MkdirAll(filepath.Dir(socket), 0o755); err != nil {
		return err
	}
	os.Remove(socket)
	ln, err := net.Listen("unix", socket)
	if err != nil {
		return err
	}
	defer ln.Close()
	uid, _ := strconv.Atoi(g.user.Uid)
	gid, _ := strconv.Atoi(g.user.Gid)
	if err := os.Chown(socket, uid, gid); err != nil {
		return err
	}
	if err := os.Chmod(socket, 0o600); err != nil {
		return err
	}
	for {
		conn, err := ln.Accept()
		if err != nil {
			return err
		}
		go func() {
			defer conn.Close()
			conn.SetDeadline(time.Now().Add(credentialWait + 35*time.Second))
			host, _ := bufio.NewReader(conn).ReadString('\n')
			json.NewEncoder(conn).Encode(g.credential(strings.TrimSpace(host)))
		}()
	}
}

// HelperCredential is a credential as a Docker credential helper gives it,
// or why there is none.
type HelperCredential struct {
	ServerURL string `json:",omitempty"`
	Username  string `json:",omitempty"`
	Secret    string `json:",omitempty"`
	Error     string `json:"error,omitempty"`
}

func (g *Guest) credential(host string) HelperCredential {
	s := g.session(credentialWait)
	if s == nil {
		return HelperCredential{Error: "not connected to Hangar"}
	}
	r, err := s.ask(Message{Type: TypeGetCredential})
	if err != nil {
		return HelperCredential{Error: err.Error()}
	}
	if !strings.EqualFold(r.Host, host) {
		return HelperCredential{Error: "Hangar's registry is " + r.Host + ", not " + host}
	}
	return HelperCredential{ServerURL: host, Username: r.Username, Secret: r.Secret}
}
