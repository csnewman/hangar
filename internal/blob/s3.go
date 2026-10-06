package blob

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/url"
	"strings"
	"time"

	"github.com/minio/minio-go/v7"
	"github.com/minio/minio-go/v7/pkg/credentials"
	"github.com/minio/minio-go/v7/pkg/encrypt"
	"github.com/minio/minio-go/v7/pkg/lifecycle"
)

// S3Config is where an S3-compatible store is and how to sign in to it.
type S3Config struct {
	// URL is the endpoint and the bucket, as http(s)://host[:port]/bucket.
	URL       string
	AccessKey string
	SecretKey string
	// Region, if the store needs one named.
	Region string
	// Encrypt asks the store to encrypt every object it writes with its
	// own keys (SSE-S3).
	Encrypt bool
	// Versioned turns on the bucket's versioning, and has the store delete
	// a version NoncurrentDays after another replaces it, and delete
	// markers left with no versions behind them.
	Versioned      bool
	NoncurrentDays int
	// Expire has the store delete objects under each prefix some days
	// after they were written.
	Expire []Expiry
}

// Expiry is objects under Prefix, deleted by the store Days after they
// were written.
type Expiry struct {
	Prefix string
	Days   int
}

// storeStartup is how long NewS3 keeps trying to reach the store.
const storeStartup = 2 * time.Minute

// S3 is a Store in one bucket of an S3-compatible object store.
type S3 struct {
	client *minio.Client
	bucket string
	sse    encrypt.ServerSide
}

// ParseS3URL reads http(s)://[key:secret@]host[:port]/bucket, the store's
// endpoint and bucket with, if given, the keys to sign in with.
func ParseS3URL(raw string) (S3Config, error) {
	u, err := url.Parse(raw)
	if err != nil {
		return S3Config{}, err
	}
	bucket := strings.Trim(u.Path, "/")
	if u.Host == "" || bucket == "" || strings.Contains(bucket, "/") || (u.Scheme != "http" && u.Scheme != "https") {
		return S3Config{}, fmt.Errorf("%q is not http(s)://host[:port]/bucket", u.Redacted())
	}
	cfg := S3Config{URL: u.Scheme + "://" + u.Host + "/" + bucket}
	if u.User != nil {
		cfg.AccessKey = u.User.Username()
		cfg.SecretKey, _ = u.User.Password()
	}
	return cfg, nil
}

// NewS3 connects to the store, making the bucket if it is not there.
func NewS3(ctx context.Context, cfg S3Config) (*S3, error) {
	parsed, err := ParseS3URL(cfg.URL)
	if err != nil {
		return nil, err
	}
	u, _ := url.Parse(parsed.URL)
	bucket := strings.Trim(u.Path, "/")
	client, err := minio.New(u.Host, &minio.Options{
		Creds:        credentials.NewStaticV4(cfg.AccessKey, cfg.SecretKey, ""),
		Secure:       u.Scheme == "https",
		Region:       cfg.Region,
		BucketLookup: minio.BucketLookupPath,
	})
	if err != nil {
		return nil, err
	}
	s := &S3{client: client, bucket: bucket}
	if cfg.Encrypt {
		s.sse = encrypt.NewSSE()
	}
	// A store starting beside the server answers before it can serve.
	var ok bool
	deadline := time.Now().Add(storeStartup)
	for wait := time.Second; ; wait = min(2*wait, 5*time.Second) {
		if ok, err = client.BucketExists(ctx, bucket); err == nil {
			break
		}
		if time.Now().After(deadline) {
			return nil, fmt.Errorf("reaching the object store at %s: %w", u.Host, err)
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(wait):
		}
	}
	if !ok {
		if err := client.MakeBucket(ctx, bucket, minio.MakeBucketOptions{Region: cfg.Region}); err != nil {
			// Another replica may have made it meanwhile.
			if ok, _ := client.BucketExists(ctx, bucket); !ok {
				return nil, fmt.Errorf("making bucket %s: %w", bucket, err)
			}
		}
	}
	if err := s.configure(ctx, cfg); err != nil {
		return nil, fmt.Errorf("setting up bucket %s: %w", bucket, err)
	}
	return s, nil
}

// configure sets the bucket's versioning and lifecycle rules as cfg asks,
// on every start, so a change to them takes effect.
func (s *S3) configure(ctx context.Context, cfg S3Config) error {
	lc := lifecycle.NewConfiguration()
	if cfg.Versioned {
		if err := s.client.EnableVersioning(ctx, s.bucket); err != nil {
			return err
		}
		days := cfg.NoncurrentDays
		if days <= 0 {
			days = 1
		}
		lc.Rules = append(lc.Rules, lifecycle.Rule{ID: "hangar-noncurrent", Status: "Enabled",
			RuleFilter:                  lifecycle.Filter{Prefix: ""},
			NoncurrentVersionExpiration: lifecycle.NoncurrentVersionExpiration{NoncurrentDays: lifecycle.ExpirationDays(days)},
			Expiration:                  lifecycle.Expiration{DeleteMarker: true}})
	}
	for i, e := range cfg.Expire {
		lc.Rules = append(lc.Rules, lifecycle.Rule{ID: fmt.Sprintf("hangar-expire-%d", i), Status: "Enabled",
			RuleFilter: lifecycle.Filter{Prefix: e.Prefix},
			Expiration: lifecycle.Expiration{Days: lifecycle.ExpirationDays(e.Days)}})
	}
	if len(lc.Rules) == 0 {
		return nil
	}
	return s.client.SetBucketLifecycle(ctx, s.bucket, lc)
}

// Rules returns the bucket's lifecycle rules, as the store has them.
func (s *S3) Rules(ctx context.Context) ([]lifecycle.Rule, error) {
	lc, err := s.client.GetBucketLifecycle(ctx, s.bucket)
	if err != nil {
		return nil, err
	}
	return lc.Rules, nil
}

func notFound(err error) error {
	if r := minio.ToErrorResponse(err); r.Code == minio.NoSuchKey || r.StatusCode == 404 {
		return ErrNotFound
	}
	return err
}

// unknownPartSize is the part an upload of unknown size is sent in, and so
// the memory it buffers.
const unknownPartSize = 16 << 20

func (s *S3) Put(ctx context.Context, key string, r io.Reader, size int64) error {
	opts := minio.PutObjectOptions{ContentType: "application/octet-stream", ServerSideEncryption: s.sse}
	if size < 0 {
		opts.PartSize = unknownPartSize
	}
	_, err := s.client.PutObject(ctx, s.bucket, key, r, size, opts)
	return err
}

type s3Reader struct {
	*minio.Object
	size int64
}

func (r s3Reader) Size() int64 { return r.size }

func (s *S3) Get(ctx context.Context, key string) (Reader, error) {
	o, err := s.client.GetObject(ctx, s.bucket, key, minio.GetObjectOptions{})
	if err != nil {
		return nil, notFound(err)
	}
	st, err := o.Stat()
	if err != nil {
		o.Close()
		return nil, notFound(err)
	}
	return s3Reader{Object: o, size: st.Size}, nil
}

func (s *S3) Stat(ctx context.Context, key string) (Object, error) {
	st, err := s.client.StatObject(ctx, s.bucket, key, minio.StatObjectOptions{})
	if err != nil {
		return Object{}, notFound(err)
	}
	return Object{Key: key, Size: st.Size, Modified: st.LastModified}, nil
}

// Copy copies within the store; past 5 GiB, a single copy request is not
// enough, and ComposeObject copies in parts.
func (s *S3) Copy(ctx context.Context, src, dst string) error {
	_, err := s.client.ComposeObject(ctx,
		minio.CopyDestOptions{Bucket: s.bucket, Object: dst, Encryption: s.sse},
		minio.CopySrcOptions{Bucket: s.bucket, Object: src})
	return notFound(err)
}

func (s *S3) Delete(ctx context.Context, key string) error {
	err := s.client.RemoveObject(ctx, s.bucket, key, minio.RemoveObjectOptions{})
	if errors.Is(notFound(err), ErrNotFound) {
		return nil
	}
	return err
}

func (s *S3) List(ctx context.Context, prefix string) ([]Object, error) {
	var out []Object
	for o := range s.client.ListObjects(ctx, s.bucket, minio.ListObjectsOptions{Prefix: prefix, Recursive: true}) {
		if o.Err != nil {
			return nil, o.Err
		}
		out = append(out, Object{Key: o.Key, Size: o.Size, Modified: o.LastModified})
	}
	return out, nil
}

func (s *S3) PutVersion(ctx context.Context, key string, r io.Reader, size int64) (string, error) {
	opts := minio.PutObjectOptions{ContentType: "application/octet-stream", ServerSideEncryption: s.sse}
	if size < 0 {
		opts.PartSize = unknownPartSize
	}
	info, err := s.client.PutObject(ctx, s.bucket, key, r, size, opts)
	if err != nil {
		return "", err
	}
	if info.VersionID == "" {
		return "", fmt.Errorf("bucket %s keeps no versions", s.bucket)
	}
	return info.VersionID, nil
}

func (s *S3) GetVersion(ctx context.Context, key, version string) (Reader, error) {
	o, err := s.client.GetObject(ctx, s.bucket, key, minio.GetObjectOptions{VersionID: version})
	if err != nil {
		return nil, notFound(err)
	}
	st, err := o.Stat()
	if err != nil {
		o.Close()
		return nil, notFound(err)
	}
	return s3Reader{Object: o, size: st.Size}, nil
}

func (s *S3) DeleteVersion(ctx context.Context, key, version string) error {
	err := s.client.RemoveObject(ctx, s.bucket, key, minio.RemoveObjectOptions{VersionID: version})
	if errors.Is(notFound(err), ErrNotFound) {
		return nil
	}
	return err
}

func (s *S3) DeleteAll(ctx context.Context, prefix string) error {
	var errs []error
	for o := range s.client.ListObjects(ctx, s.bucket, minio.ListObjectsOptions{Prefix: prefix, Recursive: true, WithVersions: true}) {
		if o.Err != nil {
			return o.Err
		}
		if err := s.DeleteVersion(ctx, o.Key, o.VersionID); err != nil {
			errs = append(errs, err)
		}
	}
	return errors.Join(errs...)
}
