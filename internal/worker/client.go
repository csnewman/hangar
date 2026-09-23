package worker

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/csnewman/hangar/internal/api"
)

// ErrRejected means the server refused a credential: the bootstrap token is
// wrong, or this worker's own was revoked or removed. Retrying cannot fix it.
var ErrRejected = errors.New("the server rejected the credential")

// client speaks the worker API. Every request is one the worker opens; the
// server never needs a route to the worker.
type client struct {
	base       string
	credential string
	http       *http.Client
}

func newClient(base string) *client {
	return &client{
		base: base,
		// Longer than the server holds a request for the desired set, so a
		// held request is never cut off from this end.
		http: &http.Client{Timeout: 60 * time.Second},
	}
}

// loadOrRegister reads the worker's credential, or exchanges the bootstrap
// token for one and saves it if there is none yet.
func (c *client) loadOrRegister(ctx context.Context, cfg *Config) error {
	b, err := os.ReadFile(cfg.Server.CredentialFile)
	if err == nil {
		c.credential = strings.TrimSpace(string(b))
		return nil
	}
	if !errors.Is(err, os.ErrNotExist) {
		return err
	}

	if cfg.Server.TokenFile == "" {
		return fmt.Errorf("no credential at %s and no server.token_file to register with", cfg.Server.CredentialFile)
	}
	tok, err := os.ReadFile(cfg.Server.TokenFile)
	if err != nil {
		return fmt.Errorf("reading the bootstrap token: %w", err)
	}
	var cred api.WorkerCredential
	req := api.RegisterWorker{Name: cfg.Node.Name, Labels: cfg.Node.Labels}
	if err := c.do(ctx, http.MethodPost, "/api/worker/v1/register", strings.TrimSpace(string(tok)), req, &cred); err != nil {
		return fmt.Errorf("registering as %s: %w", cfg.Node.Name, err)
	}

	// Written beside its final name and renamed, so a crash leaves either no
	// credential or a whole one.
	if err := os.MkdirAll(filepath.Dir(cfg.Server.CredentialFile), 0o700); err != nil {
		return err
	}
	tmp := cfg.Server.CredentialFile + ".tmp"
	if err := os.WriteFile(tmp, []byte(cred.Credential+"\n"), 0o600); err != nil {
		return err
	}
	if err := os.Rename(tmp, cfg.Server.CredentialFile); err != nil {
		return err
	}
	c.credential = cred.Credential
	return nil
}

// desired waits for a desired set newer than after. The server answers at
// once if there is one, and otherwise holds the request until there is or
// until its own timeout, when it answers with the current set regardless.
func (c *client) desired(ctx context.Context, after int64) (api.DesiredSet, error) {
	var set api.DesiredSet
	err := c.do(ctx, http.MethodGet, "/api/worker/v1/desired?after="+strconv.FormatInt(after, 10), c.credential, nil, &set)
	return set, err
}

func (c *client) report(ctx context.Context, st api.WorkerStatus) error {
	return c.do(ctx, http.MethodPut, "/api/worker/v1/status", c.credential, st, nil)
}

func (c *client) do(ctx context.Context, method, path, token string, body, out any) error {
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, c.base+path, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+token)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := c.http.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusUnauthorized {
		return ErrRejected
	}
	if resp.StatusCode >= 300 {
		var e api.Error
		b, _ := io.ReadAll(io.LimitReader(resp.Body, 64<<10))
		if json.Unmarshal(b, &e) == nil && e.Error != "" {
			return fmt.Errorf("%s %s: %s", method, path, e.Error)
		}
		return fmt.Errorf("%s %s: %s", method, path, resp.Status)
	}
	if out == nil {
		return nil
	}
	return json.NewDecoder(resp.Body).Decode(out)
}
