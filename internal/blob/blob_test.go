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

// TestExpiry checks the lifecycle rules a bucket is given, against
// HANGAR_TEST_S3_URL's store.
func TestExpiry(t *testing.T) {
	raw := os.Getenv("HANGAR_TEST_S3_URL")
	if raw == "" {
		t.Skip("HANGAR_TEST_S3_URL is not set")
	}
	cfg, err := blob.ParseS3URL(raw)
	if err != nil {
		t.Fatal(err)
	}
	cfg.URL += "-expiry"
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
		got = append(got, fmt.Sprintf("%s:%d", r.RuleFilter.Prefix, r.Expiration.Days))
	}
	if strings.Join(got, " ") != "scratch/:2" {
		t.Errorf("the bucket's rules are %v", got)
	}
}
