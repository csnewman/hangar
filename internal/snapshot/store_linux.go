//go:build linux

package snapshot

import (
	"context"
	"fmt"
	"os"
	"path/filepath"

	"github.com/containerd/containerd/v2/core/content"
	"github.com/containerd/containerd/v2/core/diff/apply"
	"github.com/containerd/containerd/v2/core/images"
	"github.com/containerd/containerd/v2/core/mount"
	"github.com/containerd/containerd/v2/core/remotes"
	"github.com/containerd/containerd/v2/core/remotes/docker"
	"github.com/containerd/containerd/v2/core/snapshots"
	"github.com/containerd/containerd/v2/pkg/namespaces"
	"github.com/containerd/containerd/v2/pkg/rootfs"
	"github.com/containerd/containerd/v2/plugins/content/local"
	"github.com/containerd/containerd/v2/plugins/snapshots/overlay"
	"github.com/containerd/platforms"
	"github.com/distribution/reference"
	"github.com/opencontainers/go-digest"
	ocispec "github.com/opencontainers/image-spec/specs-go/v1"
)

// Store holds pulled image content and the snapshots unpacked from it.
//
// Everything lives under one root so a worker owns exactly one directory:
//
//	<root>/content     blobs, addressed by digest
//	<root>/snapshots   overlayfs layers and their metadata
//	<root>/mounts      where a prepared snapshot is mounted for export
type Store struct {
	root    string
	content content.Store
	snap    snapshots.Snapshotter
}

// Open prepares a store rooted at root, creating it if needed.
func Open(root string) (*Store, error) {
	abs, err := filepath.Abs(root)
	if err != nil {
		return nil, err
	}
	for _, d := range []string{"content", "snapshots", "mounts"} {
		if err := os.MkdirAll(filepath.Join(abs, d), 0o700); err != nil {
			return nil, err
		}
	}

	cs, err := local.NewStore(filepath.Join(abs, "content"))
	if err != nil {
		return nil, fmt.Errorf("opening the content store: %w", err)
	}
	sn, err := overlay.NewSnapshotter(filepath.Join(abs, "snapshots"))
	if err != nil {
		return nil, fmt.Errorf("opening the overlay snapshotter: %w", err)
	}
	return &Store{root: abs, content: cs, snap: sn}, nil
}

// Close releases the snapshotter's metadata database.
func (s *Store) Close() error { return s.snap.Close() }

// namespace is the containerd namespace this store's metadata lives in.
//
// containerd buckets snapshot metadata by namespace and requires one on the
// context. Nothing else shares this store, so the name only has to be stable;
// it is set here rather than asked of callers, who have no reason to know
// containerd is involved at all.
const namespace = "hangar"

func (s *Store) ctx(ctx context.Context) context.Context {
	return namespaces.WithNamespace(ctx, namespace)
}

// Pull fetches ref and unpacks its layers, returning the chain ID of the
// resulting snapshot.
//
// The chain ID identifies the stack of layers rather than the image, so two
// images sharing a base share the snapshot underneath it -- which is the
// whole reason for reusing a snapshotter rather than flattening to a disk.
func (s *Store) Pull(ctx context.Context, ref string, platform string) (string, error) {
	ctx = s.ctx(ctx)
	matcher := platforms.Default()
	if platform != "" {
		p, err := platforms.Parse(platform)
		if err != nil {
			return "", fmt.Errorf("parsing platform %q: %w", platform, err)
		}
		matcher = platforms.Only(p)
	}

	// containerd wants a fully qualified reference. The familiar short forms
	// are a Docker CLI convenience -- "ubuntu:26.04" is really
	// "docker.io/library/ubuntu:26.04" -- and without expanding them the
	// resolver reports a confusing parse error about a port.
	named, err := reference.ParseDockerRef(ref)
	if err != nil {
		return "", fmt.Errorf("parsing image reference %q: %w", ref, err)
	}
	full := named.String()

	resolver := docker.NewResolver(docker.ResolverOptions{})
	name, desc, err := resolver.Resolve(ctx, full)
	if err != nil {
		return "", fmt.Errorf("resolving %s: %w", full, err)
	}
	fetcher, err := resolver.Fetcher(ctx, name)
	if err != nil {
		return "", fmt.Errorf("fetching %s: %w", ref, err)
	}

	// Walk the manifest, writing every blob it references into the content
	// store. ChildrenHandler is what follows manifest -> config and layers.
	handler := images.Handlers(
		remotes.FetchHandler(s.content, fetcher),
		images.ChildrenHandler(s.content),
	)
	if err := images.Dispatch(ctx, handler, nil, desc); err != nil {
		return "", fmt.Errorf("pulling %s: %w", full, err)
	}

	img := images.Image{Name: full, Target: desc}
	diffIDs, err := img.RootFS(ctx, s.content, matcher)
	if err != nil {
		return "", fmt.Errorf("reading the rootfs of %s: %w", full, err)
	}
	manifest, err := images.Manifest(ctx, s.content, desc, matcher)
	if err != nil {
		return "", fmt.Errorf("reading the manifest of %s: %w", full, err)
	}
	if len(manifest.Layers) != len(diffIDs) {
		return "", fmt.Errorf("%s has %d layers but %d diff ids",
			full, len(manifest.Layers), len(diffIDs))
	}

	layers := make([]rootfs.Layer, len(diffIDs))
	for i := range diffIDs {
		layers[i].Diff = ocispec.Descriptor{
			MediaType: ocispec.MediaTypeImageLayer,
			Digest:    diffIDs[i],
		}
		layers[i].Blob = manifest.Layers[i]
	}

	chainID, err := rootfs.ApplyLayers(ctx, layers, s.snap, apply.NewFileSystemApplier(s.content))
	if err != nil {
		return "", fmt.Errorf("unpacking %s: %w", full, err)
	}
	return chainID.String(), nil
}

// Mount materialises a snapshot as a directory and returns its path.
//
// The snapshot is taken as a view: read-only, so several environments can
// share one base without any of them being able to change it. Writes belong
// in the guest's own upper layer.
//
// Mounting needs CAP_SYS_ADMIN, which a worker has and a laptop shell
// usually does not.
func (s *Store) Mount(ctx context.Context, chainID, key string) (string, error) {
	ctx = s.ctx(ctx)
	d, err := digest.Parse(chainID)
	if err != nil {
		return "", fmt.Errorf("parsing chain id: %w", err)
	}
	target := filepath.Join(s.root, "mounts", key)
	if err := os.MkdirAll(target, 0o755); err != nil {
		return "", err
	}

	mounts, err := s.snap.View(ctx, key, d.String())
	if err != nil {
		// A view under this key may already exist from an earlier run.
		if mounts, err = s.snap.Mounts(ctx, key); err != nil {
			return "", fmt.Errorf("preparing a view of %s: %w", chainID, err)
		}
	}
	if err := mount.All(mounts, target); err != nil {
		return "", fmt.Errorf("mounting %s at %s: %w", chainID, target, err)
	}
	return target, nil
}

// Unmount releases a directory returned by Mount and removes the view.
func (s *Store) Unmount(ctx context.Context, key string) error {
	ctx = s.ctx(ctx)
	target := filepath.Join(s.root, "mounts", key)
	if err := mount.UnmountAll(target, 0); err != nil {
		return err
	}
	return s.snap.Remove(ctx, key)
}
