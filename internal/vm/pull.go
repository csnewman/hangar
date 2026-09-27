package vm

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"runtime"
	"strings"
	"sync/atomic"
	"time"

	"github.com/containerd/containerd/v2/core/content"
	"github.com/containerd/containerd/v2/core/images"
	"github.com/containerd/containerd/v2/core/remotes"
	"github.com/containerd/containerd/v2/core/remotes/docker"
	"github.com/containerd/containerd/v2/pkg/archive"
	"github.com/containerd/containerd/v2/pkg/archive/compression"
	"github.com/containerd/containerd/v2/plugins/content/local"
	"github.com/containerd/platforms"
	"github.com/distribution/reference"
	"github.com/opencontainers/go-digest"
	ocispec "github.com/opencontainers/image-spec/specs-go/v1"
)

// RegistryAuth is how a worker signs in to a registry to pull from it. A
// registry with none is pulled from anonymously.
type RegistryAuth struct {
	Username string
	// PasswordFile holds the password or token.
	PasswordFile string
}

// guestPlatform is the platform an image is pulled for: the host's, which a
// guest shares. The OS is Linux whatever the worker is built for, since a
// guest is always Linux.
func guestPlatform() platforms.MatchComparer {
	return platforms.Only(ocispec.Platform{OS: "linux", Architecture: runtime.GOARCH})
}

// newResolver makes the resolver pulls and lookups go through. A registry on
// this machine is spoken to over plain HTTP, as one run for testing images
// before they are published is; any other over TLS. Hangar's own registry,
// if there is one, is reached through the server, as the worker.
func newResolver(auth map[string]RegistryAuth, hangar *hangarRegistry) remotes.Resolver {
	hosts := docker.ConfigureDefaultRegistries(docker.WithPlainHTTP(docker.MatchLocalhost), docker.WithAuthorizer(docker.NewDockerAuthorizer(
		docker.WithAuthCreds(func(host string) (string, string, error) {
			a, ok := auth[host]
			if !ok {
				return "", "", nil
			}
			b, err := os.ReadFile(a.PasswordFile)
			if err != nil {
				return "", "", fmt.Errorf("the password for %s: %w", host, err)
			}
			return a.Username, strings.TrimSpace(string(b)), nil
		}),
	)))
	return docker.NewResolver(docker.ResolverOptions{
		Hosts: func(host string) ([]docker.RegistryHost, error) {
			if hangar == nil || !strings.EqualFold(host, hangar.host) {
				return hosts(host)
			}
			u, err := url.Parse(hangar.url)
			if err != nil {
				return nil, fmt.Errorf("the server's registry at %s: %w", hangar.url, err)
			}
			return []docker.RegistryHost{{
				Client: http.DefaultClient,
				Authorizer: docker.NewDockerAuthorizer(docker.WithAuthCreds(func(string) (string, string, error) {
					return "hangar-worker", hangar.credential, nil
				})),
				Host:         u.Host,
				Scheme:       u.Scheme,
				Path:         u.Path + "/v2",
				Capabilities: docker.HostCapabilityPull | docker.HostCapabilityResolve,
			}}, nil
		},
	})
}

// resolveDigest asks ref's registry for the digest of what ref names: the
// multi-platform index, where it is one. It fetches only the manifest's
// descriptor.
func resolveDigest(ctx context.Context, ref string, auth map[string]RegistryAuth, hangar *hangarRegistry) (string, error) {
	named, err := reference.ParseDockerRef(ref)
	if err != nil {
		return "", fmt.Errorf("parsing the image reference: %w", err)
	}
	_, desc, err := newResolver(auth, hangar).Resolve(ctx, named.String())
	if err != nil {
		return "", fmt.Errorf("resolving %s: %w", named, err)
	}
	return desc.Digest.String(), nil
}

// pull fetches the image ref names at dgst from its registry and unpacks
// its layers, in order, into dir as one root filesystem.
//
// The layers are applied onto a plain directory, so a whiteout in a layer
// removes what it names from those below rather than being kept as a
// marker: the result is the image's root filesystem as a container of it
// would see it. Owners, modes and extended attributes are kept, which needs
// the worker to be root.
//
// Blobs are fetched into a content store in scratch, which verifies each
// against its digest, and are removed once unpacked: the directory is the
// image from then on.
//
// report is told, a few times a second, how many bytes have been
// downloaded of those known to be needed -- the total grows once the
// manifest names the layers -- and then how many of the layers' bytes have
// been unpacked.
func pull(ctx context.Context, ref, dgst, dir, scratch string, auth map[string]RegistryAuth, hangar *hangarRegistry,
	report func(FetchProgress)) error {
	named, err := reference.ParseDockerRef(ref)
	if err != nil {
		return fmt.Errorf("parsing the image reference: %w", err)
	}
	d, err := digest.Parse(dgst)
	if err != nil {
		return fmt.Errorf("%s: %w", ref, err)
	}
	pinned, err := reference.WithDigest(reference.TrimNamed(named), d)
	if err != nil {
		return err
	}
	full := pinned.String()

	cs, err := local.NewStore(scratch)
	if err != nil {
		return err
	}
	resolver := newResolver(auth, hangar)
	name, desc, err := resolver.Resolve(ctx, full)
	if err != nil {
		return fmt.Errorf("resolving %s: %w", full, err)
	}
	fetcher, err := resolver.Fetcher(ctx, name)
	if err != nil {
		return err
	}

	// Only this platform's manifest, config and layers are fetched out of
	// a multi-platform index. Each is counted into the total as it is
	// dispatched, and its bytes as they arrive.
	var downloaded, needed atomic.Int64
	platform := guestPlatform()
	handler := images.Handlers(
		images.HandlerFunc(func(_ context.Context, d ocispec.Descriptor) ([]ocispec.Descriptor, error) {
			needed.Add(d.Size)
			return nil, nil
		}),
		remotes.FetchHandler(cs, countingFetcher{fetcher, &downloaded}),
		images.LimitManifests(images.FilterPlatforms(images.ChildrenHandler(cs), platform), platform, 1),
	)
	stop := every(250*time.Millisecond, func() {
		report(FetchProgress{Stage: StageDownload, Done: min(downloaded.Load(), needed.Load()), Total: needed.Load()})
	})
	err = images.Dispatch(ctx, handler, nil, desc)
	stop()
	if err != nil {
		return fmt.Errorf("fetching %s: %w", full, err)
	}
	manifest, err := images.Manifest(ctx, cs, desc, platform)
	if err != nil {
		return fmt.Errorf("%s: %w", full, err)
	}
	if len(manifest.Layers) == 0 {
		return fmt.Errorf("%s has no layers", full)
	}

	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	var layersSize int64
	for _, l := range manifest.Layers {
		layersSize += l.Size
	}
	var unpacked atomic.Int64
	var current atomic.Int32
	stop = every(250*time.Millisecond, func() {
		report(FetchProgress{Stage: StageUnpack, Done: unpacked.Load(), Total: layersSize,
			Layer: int(current.Load()), Layers: len(manifest.Layers)})
	})
	defer stop()
	for i, layer := range manifest.Layers {
		current.Store(int32(i + 1))
		if err := applyLayer(ctx, cs, layer, dir, &unpacked); err != nil {
			return fmt.Errorf("unpacking layer %d of %s: %w", i+1, full, err)
		}
	}
	return nil
}

// every calls fn at once, then at each interval until the returned stop is
// called, and once more then, so the last word is the final count.
func every(interval time.Duration, fn func()) (stop func()) {
	done := make(chan struct{})
	finished := make(chan struct{})
	go func() {
		defer close(finished)
		t := time.NewTicker(interval)
		defer t.Stop()
		for {
			fn()
			select {
			case <-t.C:
			case <-done:
				return
			}
		}
	}()
	return func() {
		close(done)
		<-finished
		fn()
	}
}

// countingFetcher counts the bytes a fetch reads as they arrive.
type countingFetcher struct {
	remotes.Fetcher
	n *atomic.Int64
}

func (c countingFetcher) Fetch(ctx context.Context, d ocispec.Descriptor) (io.ReadCloser, error) {
	rc, err := c.Fetcher.Fetch(ctx, d)
	if err != nil {
		return nil, err
	}
	return countingReadCloser{rc, c.n}, nil
}

type countingReadCloser struct {
	io.ReadCloser
	n *atomic.Int64
}

func (c countingReadCloser) Read(p []byte) (int, error) {
	n, err := c.ReadCloser.Read(p)
	c.n.Add(int64(n))
	return n, err
}

func applyLayer(ctx context.Context, cs content.Store, layer ocispec.Descriptor, dir string, unpacked *atomic.Int64) error {
	ra, err := cs.ReaderAt(ctx, layer)
	if err != nil {
		return err
	}
	defer ra.Close()
	rd, err := compression.DecompressStream(countingReadCloser{io.NopCloser(content.NewReader(ra)), unpacked})
	if err != nil {
		return err
	}
	defer rd.Close()
	if _, err := archive.Apply(ctx, dir, rd); err != nil {
		return err
	}
	return nil
}
