package image

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"time"

	"github.com/containerd/containerd/v2/pkg/archive"
	"github.com/containerd/containerd/v2/pkg/archive/compression"
	"github.com/opencontainers/go-digest"
	specs "github.com/opencontainers/image-spec/specs-go"
	ocispec "github.com/opencontainers/image-spec/specs-go/v1"
)

// OCIOptions controls WriteOCI.
type OCIOptions struct {
	// Rootfs is each tier's root filesystem, by tier, as a build made them.
	Rootfs map[string]string
	// Arch is the images' architecture, in Go's naming: arm64, amd64.
	Arch string
	// Created stamps the images' configurations.
	Created time.Time
}

// WriteOCI writes an OCI image layout at dir holding an image per tier, each
// named in the index by its tier. The first tier is one layer; each after it
// is the tier before plus a layer of what it adds, so a registry stores the
// layers they share once and a worker that pulls both fetches them once.
//
// The trees are read as they are, owners and all, so it runs as root.
func WriteOCI(ctx context.Context, dir string, o OCIOptions) error {
	blobs := filepath.Join(dir, "blobs", "sha256")
	if err := os.MkdirAll(blobs, 0o755); err != nil {
		return err
	}
	if err := writeJSON(filepath.Join(dir, ocispec.ImageLayoutFile), ocispec.ImageLayout{Version: ocispec.ImageLayoutVersion}); err != nil {
		return err
	}

	lower, err := os.MkdirTemp("", "hangar-oci-empty-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(lower)

	platform := ocispec.Platform{OS: "linux", Architecture: o.Arch}
	var layers []ocispec.Descriptor
	var diffIDs []digest.Digest
	index := ocispec.Index{Versioned: specs.Versioned{SchemaVersion: 2}, MediaType: ocispec.MediaTypeImageIndex}
	for _, tier := range Tiers {
		upper, ok := o.Rootfs[tier]
		if !ok {
			return fmt.Errorf("no root filesystem for %s", tier)
		}
		layer, diffID, err := writeLayer(ctx, blobs, lower, upper)
		if err != nil {
			return fmt.Errorf("%s: %w", tier, err)
		}
		layers = append(layers, layer)
		diffIDs = append(diffIDs, diffID)
		lower = upper

		created := o.Created
		config, err := writeBlob(blobs, ocispec.MediaTypeImageConfig, ocispec.Image{
			Created:  &created,
			Platform: platform,
			RootFS:   ocispec.RootFS{Type: "layers", DiffIDs: append([]digest.Digest(nil), diffIDs...)},
		})
		if err != nil {
			return err
		}
		manifest, err := writeBlob(blobs, ocispec.MediaTypeImageManifest, ocispec.Manifest{
			Versioned: specs.Versioned{SchemaVersion: 2},
			MediaType: ocispec.MediaTypeImageManifest,
			Config:    config,
			Layers:    append([]ocispec.Descriptor(nil), layers...),
		})
		if err != nil {
			return err
		}
		manifest.Platform = &platform
		manifest.Annotations = map[string]string{ocispec.AnnotationRefName: tier}
		index.Manifests = append(index.Manifests, manifest)
	}
	return writeJSON(filepath.Join(dir, "index.json"), index)
}

// writeLayer writes the difference from lower to upper as a gzipped layer
// blob, and gives its descriptor and the digest of its uncompressed tar.
func writeLayer(ctx context.Context, blobs, lower, upper string) (ocispec.Descriptor, digest.Digest, error) {
	f, err := os.CreateTemp(blobs, ".layer-")
	if err != nil {
		return ocispec.Descriptor{}, "", err
	}
	defer os.Remove(f.Name())
	defer f.Close()

	compressed := digest.Canonical.Digester()
	counter := &countWriter{w: io.MultiWriter(f, compressed.Hash())}
	gz, err := compression.CompressStream(counter, compression.Gzip)
	if err != nil {
		return ocispec.Descriptor{}, "", err
	}
	uncompressed := digest.Canonical.Digester()
	if err := archive.WriteDiff(ctx, io.MultiWriter(gz, uncompressed.Hash()), lower, upper); err != nil {
		gz.Close()
		return ocispec.Descriptor{}, "", err
	}
	if err := gz.Close(); err != nil {
		return ocispec.Descriptor{}, "", err
	}
	if err := f.Chmod(0o644); err != nil {
		return ocispec.Descriptor{}, "", err
	}
	if err := f.Close(); err != nil {
		return ocispec.Descriptor{}, "", err
	}
	d := compressed.Digest()
	if err := os.Rename(f.Name(), filepath.Join(blobs, d.Encoded())); err != nil {
		return ocispec.Descriptor{}, "", err
	}
	return ocispec.Descriptor{MediaType: ocispec.MediaTypeImageLayerGzip, Digest: d, Size: counter.n}, uncompressed.Digest(), nil
}

// writeBlob writes v as a JSON blob and gives its descriptor.
func writeBlob(blobs, mediaType string, v any) (ocispec.Descriptor, error) {
	b, err := json.Marshal(v)
	if err != nil {
		return ocispec.Descriptor{}, err
	}
	d := digest.FromBytes(b)
	if err := os.WriteFile(filepath.Join(blobs, d.Encoded()), b, 0o644); err != nil {
		return ocispec.Descriptor{}, err
	}
	return ocispec.Descriptor{MediaType: mediaType, Digest: d, Size: int64(len(b))}, nil
}

func writeJSON(path string, v any) error {
	b, err := json.Marshal(v)
	if err != nil {
		return err
	}
	return os.WriteFile(path, b, 0o644)
}

type countWriter struct {
	w io.Writer
	n int64
}

func (c *countWriter) Write(p []byte) (int, error) {
	n, err := c.w.Write(p)
	c.n += int64(n)
	return n, err
}
