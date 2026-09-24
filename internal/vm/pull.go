package vm

import (
	"context"
	"fmt"
	"os"
	"runtime"
	"strings"

	"github.com/containerd/containerd/v2/core/content"
	"github.com/containerd/containerd/v2/core/images"
	"github.com/containerd/containerd/v2/core/remotes"
	"github.com/containerd/containerd/v2/core/remotes/docker"
	"github.com/containerd/containerd/v2/pkg/archive"
	"github.com/containerd/containerd/v2/pkg/archive/compression"
	"github.com/containerd/containerd/v2/plugins/content/local"
	"github.com/containerd/platforms"
	"github.com/distribution/reference"
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

// pull fetches an image from its registry and unpacks its layers, in order,
// into dir as one root filesystem.
//
// The layers are applied onto a plain directory, so a whiteout in a layer
// removes what it names from those below rather than being kept as a
// marker: the result is the image's root filesystem as a container of it
// would see it. Owners, modes and extended attributes are kept, which needs
// the worker to be root.
//
// Blobs are fetched into a content store in scratch, which verifies each
// against its digest, and are removed once unpacked: the directory is the
// image from then on. It returns the digest pulled.
func pull(ctx context.Context, ref, dir, scratch string, auth map[string]RegistryAuth) (string, error) {
	named, err := reference.ParseDockerRef(ref)
	if err != nil {
		return "", fmt.Errorf("parsing the image reference: %w", err)
	}
	full := named.String()

	cs, err := local.NewStore(scratch)
	if err != nil {
		return "", err
	}
	resolver := docker.NewResolver(docker.ResolverOptions{
		Hosts: docker.ConfigureDefaultRegistries(docker.WithAuthorizer(docker.NewDockerAuthorizer(
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
		))),
	})
	name, desc, err := resolver.Resolve(ctx, full)
	if err != nil {
		return "", fmt.Errorf("resolving %s: %w", full, err)
	}
	fetcher, err := resolver.Fetcher(ctx, name)
	if err != nil {
		return "", err
	}

	// Only this platform's manifest, config and layers are fetched out of
	// a multi-platform index.
	platform := guestPlatform()
	handler := images.Handlers(
		remotes.FetchHandler(cs, fetcher),
		images.LimitManifests(images.FilterPlatforms(images.ChildrenHandler(cs), platform), platform, 1),
	)
	if err := images.Dispatch(ctx, handler, nil, desc); err != nil {
		return "", fmt.Errorf("fetching %s: %w", full, err)
	}
	manifest, err := images.Manifest(ctx, cs, desc, platform)
	if err != nil {
		return "", fmt.Errorf("%s: %w", full, err)
	}
	if len(manifest.Layers) == 0 {
		return "", fmt.Errorf("%s has no layers", full)
	}

	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", err
	}
	for i, layer := range manifest.Layers {
		if err := applyLayer(ctx, cs, layer, dir); err != nil {
			return "", fmt.Errorf("unpacking layer %d of %s: %w", i+1, full, err)
		}
	}
	return desc.Digest.String(), nil
}

func applyLayer(ctx context.Context, cs content.Store, layer ocispec.Descriptor, dir string) error {
	ra, err := cs.ReaderAt(ctx, layer)
	if err != nil {
		return err
	}
	defer ra.Close()
	rd, err := compression.DecompressStream(content.NewReader(ra))
	if err != nil {
		return err
	}
	defer rd.Close()
	if _, err := archive.Apply(ctx, dir, rd); err != nil {
		return err
	}
	return nil
}
