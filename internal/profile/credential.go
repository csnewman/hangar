package profile

import (
	"encoding/base64"
	"encoding/json"
	"errors"
	"strings"
	"time"
)

// A registry credential is what an environment's agent is given to push to
// and pull from Hangar's registry as its owner: who it is for, which
// environment asked, and when it stops working, sealed with the server's
// key. Nothing about it is stored, and it is good for an hour.

// RegistryCredentialPrefix begins every registry credential, so the
// registry tells one from an access token.
const RegistryCredentialPrefix = "hge_"

// RegistryCredentialLifetime is how long a registry credential works.
const RegistryCredentialLifetime = time.Hour

const registryCredentialBinding = "registry-credential"

type registryClaims struct {
	User        string `json:"u"`
	Environment string `json:"e"`
	Expires     int64  `json:"x"`
}

// ErrBadCredential is returned for a registry credential that does not
// open, or has expired.
var ErrBadCredential = errors.New("not a valid registry credential")

// IssueRegistryCredential makes a credential for a user, asked for by one
// of their environments.
func (s *Store) IssueRegistryCredential(userID, environment string) (string, error) {
	plain, err := json.Marshal(registryClaims{User: userID, Environment: environment,
		Expires: time.Now().Add(RegistryCredentialLifetime).Unix()})
	if err != nil {
		return "", err
	}
	sealed, err := s.sealer.seal(plain, registryCredentialBinding)
	if err != nil {
		return "", err
	}
	return RegistryCredentialPrefix + base64.RawURLEncoding.EncodeToString(sealed), nil
}

// OpenRegistryCredential returns the user a registry credential is for.
func (s *Store) OpenRegistryCredential(credential string) (string, error) {
	enc, ok := strings.CutPrefix(credential, RegistryCredentialPrefix)
	if !ok {
		return "", ErrBadCredential
	}
	sealed, err := base64.RawURLEncoding.DecodeString(enc)
	if err != nil {
		return "", ErrBadCredential
	}
	plain, err := s.sealer.open(sealed, registryCredentialBinding)
	if err != nil {
		return "", ErrBadCredential
	}
	var c registryClaims
	if json.Unmarshal(plain, &c) != nil || c.User == "" || time.Now().Unix() >= c.Expires {
		return "", ErrBadCredential
	}
	return c.User, nil
}
