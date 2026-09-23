package users

import (
	"crypto/rand"
	"crypto/subtle"
	"encoding/base64"
	"errors"
	"fmt"
	"strings"

	"golang.org/x/crypto/argon2"
)

// Argon2id parameters for new hashes: OWASP's recommended minimum of 19 MiB,
// two passes, one lane. Each hash records the parameters it was made with,
// so these can rise without invalidating existing passwords.
const (
	argonMemoryKiB = 19 * 1024
	argonTime      = 2
	argonThreads   = 1
	argonKeyLen    = 32
	argonSaltLen   = 16
)

// MinPasswordLength is the shortest password accepted.
const MinPasswordLength = 8

// hashPassword returns an argon2id hash in PHC string form:
// $argon2id$v=19$m=...,t=...,p=...$salt$key
func hashPassword(password string) (string, error) {
	salt := make([]byte, argonSaltLen)
	if _, err := rand.Read(salt); err != nil {
		return "", err
	}
	key := argon2.IDKey([]byte(password), salt, argonTime, argonMemoryKiB, argonThreads, argonKeyLen)
	return fmt.Sprintf("$argon2id$v=%d$m=%d,t=%d,p=%d$%s$%s", argon2.Version, argonMemoryKiB, argonTime,
		argonThreads, b64.EncodeToString(salt), b64.EncodeToString(key)), nil
}

var b64 = base64.RawStdEncoding

var errBadHash = errors.New("malformed password hash")

// checkPassword reports whether password matches a hash from hashPassword.
func checkPassword(hash, password string) (bool, error) {
	parts := strings.Split(hash, "$")
	if len(parts) != 6 || parts[1] != "argon2id" {
		return false, errBadHash
	}
	var version int
	if _, err := fmt.Sscanf(parts[2], "v=%d", &version); err != nil || version != argon2.Version {
		return false, errBadHash
	}
	var mem, time uint32
	var threads uint8
	if _, err := fmt.Sscanf(parts[3], "m=%d,t=%d,p=%d", &mem, &time, &threads); err != nil {
		return false, errBadHash
	}
	salt, err := b64.DecodeString(parts[4])
	if err != nil {
		return false, errBadHash
	}
	want, err := b64.DecodeString(parts[5])
	if err != nil {
		return false, errBadHash
	}
	got := argon2.IDKey([]byte(password), salt, time, mem, threads, uint32(len(want)))
	return subtle.ConstantTimeCompare(got, want) == 1, nil
}

// dummyHash is checked against when a sign-in names a user who does not
// exist or has no password, so that answering takes as long as it does for a
// real user and the time taken does not reveal which usernames exist.
var dummyHash = func() string {
	h, err := hashPassword("hangar dummy password")
	if err != nil {
		panic(err)
	}
	return h
}()
