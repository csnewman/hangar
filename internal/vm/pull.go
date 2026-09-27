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
// the layers the store does not have into it, returning the image's layers,
// bottom first, and a func releasing the claim held on them meanwhile.
//
// Layers are unpacked each on its own, with overlayfs's whiteouts, for the
// store to stack. Owners, modes and extended attributes are kept, which
// needs the worker to be root.
//
// Blobs are fetched into a content store in scratch, which verifies each
// against its digest; the caller removes it once the layers are unpacked.
// A layer the store already has is not fetched at all.
//
// report is told, a few times a second, how many bytes have been
// downloaded of those known to be needed -- the total grows once the
// manifest names the layers -- and then how many of the new layers' bytes
// have been unpacked.
func pull(ctx context.Context, ref, dgst, scratch string, auth map[string]RegistryAuth, hangar *hangarRegistry,
	store *ImageStore, report func(FetchProgress)) ([]digest.Digest, func(), error) {
	release := func() {}
	named, err := reference.ParseDockerRef(ref)
	if err != nil {
		return nil, release, fmt.Errorf("parsing the image reference: %w", err)
	}
	d, err := digest.Parse(dgst)
	if err != nil {
		return nil, release, fmt.Errorf("%s: %w", ref, err)
	}
	pinned, err := reference.WithDigest(reference.TrimNamed(named), d)
	if err != nil {
		return nil, release, err
	}
	full := pinned.String()

	cs, err := local.NewStore(scratch)
	if err != nil {
		return nil, release, err
	}
	resolver := newResolver(auth, hangar)
	name, desc, err := resolver.Resolve(ctx, full)
	if err != nil {
		return nil, release, fmt.Errorf("resolving %s: %w", full, err)
	}
	fetcher, err := resolver.Fetcher(ctx, name)
	if err != nil {
		return nil, release, err
	}

	// Only this platform's manifest, config and layers are fetched out of
	// a multi-platform index, and of its layers only those the store does
	// not have. Each is counted into the total as it is dispatched, and its
	// bytes as they arrive.
	var downloaded, needed atomic.Int64
	platform := guestPlatform()
	fetch := remotes.FetchHandler(cs, countingFetcher{fetcher, &downloaded})
	children := images.LimitManifests(images.FilterPlatforms(images.ChildrenHandler(cs), platform), platform, 1)
	handler := images.HandlerFunc(func(ctx context.Context, d ocispec.Descriptor) ([]ocispec.Descriptor, error) {
		if images.IsLayerType(d.MediaType) && store.hasLayer(d.Digest) {
			return nil, nil
		}
		needed.Add(d.Size)
		if _, err := fetch(ctx, d); err != nil {
			return nil, err
		}
		return children(ctx, d)
	})
	stop := every(250*time.Millisecond, func() {
		report(FetchProgress{Stage: StageDownload, Done: min(downloaded.Load(), needed.Load()), Total: needed.Load()})
	})
	err = images.Dispatch(ctx, handler, nil, desc)
	stop()
	if err != nil {
		return nil, release, fmt.Errorf("fetching %s: %w", full, err)
	}
	manifest, err := images.Manifest(ctx, cs, desc, platform)
	if err != nil {
		return nil, release, fmt.Errorf("%s: %w", full, err)
	}
	if len(manifest.Layers) == 0 {
		return nil, release, fmt.Errorf("%s has no layers", full)
	}

	layers := make([]digest.Digest, len(manifest.Layers))
	var fresh []ocispec.Descriptor
	var layersSize int64
	for i, l := range manifest.Layers {
		layers[i] = l.Digest
		if !store.hasLayer(l.Digest) {
			fresh = append(fresh, l)
			layersSize += l.Size
		}
	}
	release = store.claimLayers(layers)
	var unpacked atomic.Int64
	var current atomic.Int32
	stop = every(250*time.Millisecond, func() {
		report(FetchProgress{Stage: StageUnpack, Done: unpacked.Load(), Total: layersSize,
			Layer: int(current.Load()), Layers: len(fresh)})
	})
	defer stop()
	for i, layer := range fresh {
		current.Store(int32(i + 1))
		if err := applyLayer(ctx, cs, layer, store, &unpacked); err != nil {
			return nil, release, fmt.Errorf("unpacking layer %s of %s: %w", layer.Digest, full, err)
		}
	}
	return layers, release, nil
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

func applyLayer(ctx context.Context, cs content.Store, layer ocispec.Descriptor, store *ImageStore, unpacked *atomic.Int64) error {
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
	return store.addLayer(ctx, layer.Digest, rd)
}
