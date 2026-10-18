package vm

import (
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"

	"github.com/csnewman/hangar/internal/erofs"
)

// ImageInfoPath is where an image says what it offers, inside its root
// filesystem. Hangar's images write it (images/common/*/tree); an image built
// some other way may not.
const ImageInfoPath = "usr/share/hangar/image.json"

// ImageInfo is what an image offers beyond booting.
type ImageInfo struct {
	// Desktop is whether the image has a desktop to run: its compositor, the
	// unit that starts it and the VNC server the Desktop tab reaches.
	Desktop bool `json:"desktop"`
	// Build identifies the build, so a worker copying a local build tells
	// one from the next. `hangar build` sets it.
	Build string `json:"build,omitempty"`
}

// ReadImageInfo reads what the image whose root filesystem is rootfs offers.
// It reports false, and no error, for an image that does not say.
func ReadImageInfo(rootfs string) (ImageInfo, bool, error) {
	return readImageInfo(os.DirFS(rootfs))
}

// imageInfo reads what an image offers: from its root filesystem, or from
// its EROFS image for a pulled copy, which has no other.
func imageInfo(img Image) (ImageInfo, bool, error) {
	if img.Base != "" {
		return ReadImageInfo(img.Base)
	}
	f, err := os.Open(filepath.Join(img.Copy, erofsFile))
	if err != nil {
		return ImageInfo{}, false, err
	}
	defer f.Close()
	im, err := erofs.Open(f)
	if err != nil {
		return ImageInfo{}, false, err
	}
	return readImageInfo(im.FS())
}

func readImageInfo(root fs.FS) (ImageInfo, bool, error) {
	var info ImageInfo
	b, err := fs.ReadFile(root, ImageInfoPath)
	if errors.Is(err, fs.ErrNotExist) {
		return info, false, nil
	}
	if err != nil {
		return info, false, err
	}
	if err := json.Unmarshal(b, &info); err != nil {
		return info, false, fmt.Errorf("%s: %w", ImageInfoPath, err)
	}
	return info, true, nil
}
