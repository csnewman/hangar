package profile

import (
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"strings"
)

// ErrNoKey is returned for a secret when the server has no key to encrypt
// it with.
var ErrNoKey = errors.New("the server has no secret key (HANGAR_SECRET_KEY_FILE), so it cannot keep credentials")

// Sealer encrypts secrets at rest with AES-256-GCM. Each sealed value is
// bound to what it is -- the user and path it belongs to -- so a value
// copied to another row does not open.
type Sealer struct {
	aead cipher.AEAD
}

// sealVersion prefixes every sealed value, for a change of scheme or key.
const sealVersion = 1

// LoadSealer reads a 32-byte key, as hex or base64, from a file.
func LoadSealer(file string) (*Sealer, error) {
	b, err := os.ReadFile(file)
	if err != nil {
		return nil, err
	}
	s := strings.TrimSpace(string(b))
	key, err := hex.DecodeString(s)
	if err != nil {
		key, err = base64.StdEncoding.DecodeString(s)
	}
	if err != nil || len(key) != 32 {
		return nil, fmt.Errorf("%s: the secret key must be 32 bytes, as hex or base64 (openssl rand -hex 32)", file)
	}
	return NewSealer(key)
}

// NewSealer makes a sealer from a 32-byte key.
func NewSealer(key []byte) (*Sealer, error) {
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	aead, err := cipher.NewGCM(block)
	if err != nil {
		return nil, err
	}
	return &Sealer{aead: aead}, nil
}

func (s *Sealer) seal(plain []byte, bound string) ([]byte, error) {
	if s == nil {
		return nil, ErrNoKey
	}
	nonce := make([]byte, s.aead.NonceSize())
	if _, err := rand.Read(nonce); err != nil {
		return nil, err
	}
	out := append([]byte{sealVersion}, nonce...)
	return s.aead.Seal(out, nonce, plain, []byte(bound)), nil
}

func (s *Sealer) open(sealed []byte, bound string) ([]byte, error) {
	if s == nil {
		return nil, ErrNoKey
	}
	n := s.aead.NonceSize()
	if len(sealed) < 1+n || sealed[0] != sealVersion {
		return nil, errors.New("a sealed value is malformed")
	}
	plain, err := s.aead.Open(nil, sealed[1:1+n], sealed[1+n:], []byte(bound))
	if err != nil {
		return nil, errors.New("a sealed value does not open with the server's key")
	}
	return plain, nil
}
