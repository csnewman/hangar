package image_test

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	"github.com/containerd/containerd/v2/pkg/archive"
	"github.com/containerd/containerd/v2/pkg/archive/compression"
	ocispec "github.com/opencontainers/image-spec/specs-go/v1"

	"github.com/csnewman/hangar/internal/image"
)

func TestWriteOCI(t *testing.T) {
	ctx := context.Background()
	root := t.TempDir()
	tree := func(name string, files map[string]string) string {
		dir := filepath.Join(root, name)
		for p, content := range files {
			full := filepath.Join(dir, p)
			if err := os.MkdirAll(filepath.Dir(full), 0o755); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(full, []byte(content), 0o644); err != nil {
				t.Fatal(err)
			}
		}
		return dir
	}
	minimal := tree("minimal", map[string]string{
		"etc/os-release":     "minimal",
		"usr/bin/git":        "git",
		"usr/share/gone.txt": "removed by base",
	})
	base := tree("base", map[string]string{
		"etc/os-release":  "base",
		"usr/bin/git":     "git",
		"usr/bin/labwc":   "labwc",
		"usr/share/.keep": "",
	})

	layout := filepath.Join(root, "oci")
	err := image.WriteOCI(ctx, layout, image.OCIOptions{
		Rootfs:  map[string]string{"minimal": minimal, "base": base},
		Arch:    runtime.GOARCH,
		Created: time.Unix(0, 0).UTC(),
	})
	if err != nil {
		t.Fatal(err)
	}

	var index ocispec.Index
	readJSON(t, filepath.Join(layout, "index.json"), &index)
	manifests := map[string]ocispec.Manifest{}
	for _, d := range index.Manifests {
		var m ocispec.Manifest
		readJSON(t, blob(layout, d), &m)
		manifests[d.Annotations[ocispec.AnnotationRefName]] = m
	}
	if len(manifests["minimal"].Layers) != 1 || len(manifests["base"].Layers) != 2 {
		t.Fatalf("minimal has %d layers and base %d, want 1 and 2", len(manifests["minimal"].Layers), len(manifests["base"].Layers))
	}
	if manifests["base"].Layers[0].Digest != manifests["minimal"].Layers[0].Digest {
		t.Error("base does not share minimal's layer")
	}

	// Each image, unpacked the way a worker unpacks it, is its tree.
	for name, want := range map[string]map[string]string{
		"minimal": {"etc/os-release": "minimal", "usr/share/gone.txt": "removed by base"},
		"base":    {"etc/os-release": "base", "usr/bin/labwc": "labwc", "usr/bin/git": "git"},
	} {
		dir := filepath.Join(root, "unpacked-"+name)
		if err := os.Mkdir(dir, 0o755); err != nil {
			t.Fatal(err)
		}
		for _, l := range manifests[name].Layers {
			f, err := os.Open(blob(layout, l))
			if err != nil {
				t.Fatal(err)
			}
			r, err := compression.DecompressStream(f)
			if err != nil {
				t.Fatal(err)
			}
			if _, err := archive.Apply(ctx, dir, r); err != nil {
				t.Fatalf("%s: applying a layer: %v", name, err)
			}
			r.Close()
			f.Close()
		}
		for p, content := range want {
			if b, err := os.ReadFile(filepath.Join(dir, p)); err != nil || string(b) != content {
				t.Errorf("%s: %s is %q, %v; want %q", name, p, b, err, content)
			}
		}
		if name == "base" {
			if _, err := os.Stat(filepath.Join(dir, "usr/share/gone.txt")); !os.IsNotExist(err) {
				t.Errorf("base still has a file it removed: %v", err)
			}
		}
	}
}

func blob(layout string, d ocispec.Descriptor) string {
	return filepath.Join(layout, "blobs", d.Digest.Algorithm().String(), d.Digest.Encoded())
}

func readJSON(t *testing.T, path string, v any) {
	t.Helper()
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(b, v); err != nil {
		t.Fatal(err)
	}
}
