package blob_test

import (
	"bytes"
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"os"
	"strings"
	"testing"

	"github.com/csnewman/hangar/internal/blob"
)

// stores are the stores to hold to the contract: memory always, and an S3
// one at HANGAR_TEST_S3_URL (http://key:secret@host:port/bucket) if set,
// and with encryption by the store too if HANGAR_TEST_S3_ENCRYPT is true.
func stores(t *testing.T) map[string]blob.Store {
	out := map[string]blob.Store{"memory": blob.NewMemory()}
	raw := os.Getenv("HANGAR_TEST_S3_URL")
	if raw == "" {
		return out
	}
	cfg, err := blob.ParseS3URL(raw)
	if err != nil {
		t.Fatal(err)
	}
	encs := []bool{false}
	if os.Getenv("HANGAR_TEST_S3_ENCRYPT") == "true" {
		encs = append(encs, true)
	}
	for _, enc := range encs {
		c := cfg
		c.Encrypt = enc
		s, err := blob.NewS3(context.Background(), c)
		if err != nil {
			t.Fatal(err)
		}
		name := "s3"
		if enc {
			name = "s3-encrypted"
		}
		out[name] = s
	}
	return out
}

func TestContract(t *testing.T) {
	ctx := context.Background()
	for name, s := range stores(t) {
		t.Run(name, func(t *testing.T) {
			p := "contract-" + rand.Text() + "/"
			if _, err := s.Get(ctx, p+"missing"); !errors.Is(err, blob.ErrNotFound) {
				t.Errorf("get of a missing object: %v", err)
			}
			if _, err := s.Stat(ctx, p+"missing"); !errors.Is(err, blob.ErrNotFound) {
				t.Errorf("stat of a missing object: %v", err)
			}
			if err := s.Delete(ctx, p+"missing"); err != nil {
				t.Errorf("delete of a missing object: %v", err)
			}

			big := make([]byte, 7<<20)
			rand.Read(big)
			if err := s.Put(ctx, p+"b/big", bytes.NewReader(big), -1); err != nil {
				t.Fatal(err)
			}
			if err := s.Put(ctx, p+"a", strings.NewReader("hello"), 5); err != nil {
				t.Fatal(err)
			}
			if err := s.Put(ctx, p+"empty", strings.NewReader(""), 0); err != nil {
				t.Fatal(err)
			}
			if got, err := blob.ReadAll(ctx, s, p+"a"); err != nil || string(got) != "hello" {
				t.Errorf("read back %q, %v", got, err)
			}
			if got, err := blob.ReadAll(ctx, s, p+"empty"); err != nil || len(got) != 0 {
				t.Errorf("read back an empty object: %q, %v", got, err)
			}

			r, err := s.Get(ctx, p+"b/big")
			if err != nil {
				t.Fatal(err)
			}
			if r.Size() != int64(len(big)) {
				t.Errorf("size %d, want %d", r.Size(), len(big))
			}
			if _, err := r.Seek(5<<20, io.SeekStart); err != nil {
				t.Fatal(err)
			}
			tail, err := io.ReadAll(r)
			r.Close()
			if err != nil || !bytes.Equal(tail, big[5<<20:]) {
				t.Errorf("read from an offset: %d bytes, %v", len(tail), err)
			}

			if err := s.Copy(ctx, p+"b/big", p+"c"); err != nil {
				t.Fatal(err)
			}
			if got, err := blob.ReadAll(ctx, s, p+"c"); err != nil || !bytes.Equal(got, big) {
				t.Errorf("copy: %d bytes, %v", len(got), err)
			}
			if st, err := s.Stat(ctx, p+"c"); err != nil || st.Size != int64(len(big)) || st.Modified.IsZero() {
				t.Errorf("stat of the copy: %+v, %v", st, err)
			}

			list, err := s.List(ctx, p)
			if err != nil {
				t.Fatal(err)
			}
			var keys []string
			for _, o := range list {
				keys = append(keys, strings.TrimPrefix(o.Key, p))
			}
			if got := strings.Join(keys, " "); got != "a b/big c empty" {
				t.Errorf("listed %s", got)
			}
			if list[0].Size != 5 {
				t.Errorf("listed size %d", list[0].Size)
			}

			for _, k := range []string{"a", "b/big", "c", "empty"} {
				if err := s.Delete(ctx, p+k); err != nil {
					t.Fatal(err)
				}
			}
			if list, err := s.List(ctx, p); err != nil || len(list) != 0 {
				t.Errorf("left after deleting: %v, %v", list, err)
			}
		})
	}
}

func TestParseS3URL(t *testing.T) {
	cfg, err := blob.ParseS3URL("https://key:sec%2Fret@s3.example.com:9000/bucket")
	if err != nil {
		t.Fatal(err)
	}
	if cfg.URL != "https://s3.example.com:9000/bucket" || cfg.AccessKey != "key" || cfg.SecretKey != "sec/ret" {
		t.Errorf("parsed %+v", cfg)
	}
	for _, bad := range []string{"s3.example.com/bucket", "http://host", "http://host/a/b", "ftp://host/b"} {
		if _, err := blob.ParseS3URL(bad); err == nil {
			t.Errorf("%s was accepted", bad)
		}
	}
}

// versioned are the versioned stores to hold to the contract: memory
// always, and a versioned bucket beside HANGAR_TEST_S3_URL's if set.
func versioned(t *testing.T) map[string]blob.Versioned {
	out := map[string]blob.Versioned{"memory": blob.NewMemory()}
	raw := os.Getenv("HANGAR_TEST_S3_URL")
	if raw == "" {
		return out
	}
	cfg, err := blob.ParseS3URL(raw)
	if err != nil {
		t.Fatal(err)
	}
	cfg.URL += "-versioned"
	cfg.Versioned, cfg.NoncurrentDays = true, 3
	cfg.Expire = []blob.Expiry{{Prefix: "scratch/", Days: 2}}
	s, err := blob.NewS3(context.Background(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	rules, err := s.Rules(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	var got []string
	for _, r := range rules {
		got = append(got, fmt.Sprintf("%s:%d/%d", r.RuleFilter.Prefix, r.NoncurrentVersionExpiration.NoncurrentDays, r.Expiration.Days))
	}
	if strings.Join(got, " ") != ":3/0 scratch/:0/2" {
		t.Errorf("the bucket's rules are %v", got)
	}
	out["s3"] = s
	return out
}

func TestVersionedContract(t *testing.T) {
	ctx := context.Background()
	for name, s := range versioned(t) {
		t.Run(name, func(t *testing.T) {
			k := "contract-" + rand.Text() + "/dir/file"
			v1, err := s.PutVersion(ctx, k, strings.NewReader("one"), 3)
			if err != nil {
				t.Fatal(err)
			}
			v2, err := s.PutVersion(ctx, k, strings.NewReader("two"), -1)
			if err != nil {
				t.Fatal(err)
			}
			if v1 == v2 || v1 == "" {
				t.Fatalf("versions %q and %q", v1, v2)
			}
			for v, want := range map[string]string{v1: "one", v2: "two"} {
				if got, err := blob.ReadVersion(ctx, s, k, v); err != nil || string(got) != want {
					t.Errorf("version %s: %q, %v", v, got, err)
				}
			}
			if err := s.DeleteVersion(ctx, k, v1); err != nil {
				t.Fatal(err)
			}
			if _, err := s.GetVersion(ctx, k, v1); !errors.Is(err, blob.ErrNotFound) {
				t.Errorf("a deleted version: %v", err)
			}
			if got, err := blob.ReadVersion(ctx, s, k, v2); err != nil || string(got) != "two" {
				t.Errorf("the other version after deleting one: %q, %v", got, err)
			}
			if err := s.DeleteVersion(ctx, k, v1); err != nil {
				t.Errorf("deleting a deleted version: %v", err)
			}
			other := strings.TrimSuffix(k, "dir/file") + "other"
			if _, err := s.PutVersion(ctx, other, strings.NewReader("x"), 1); err != nil {
				t.Fatal(err)
			}
			if err := s.DeleteAll(ctx, strings.TrimSuffix(k, "dir/file")); err != nil {
				t.Fatal(err)
			}
			if _, err := s.GetVersion(ctx, k, v2); !errors.Is(err, blob.ErrNotFound) {
				t.Errorf("after deleting all: %v", err)
			}
		})
	}
}
