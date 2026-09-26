package vm_test

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/csnewman/hangar/internal/vm"
)

func TestReadImageInfo(t *testing.T) {
	write := func(t *testing.T, content string) string {
		root := t.TempDir()
		p := filepath.Join(root, vm.ImageInfoPath)
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
		return root
	}

	info, known, err := vm.ReadImageInfo(write(t, `{"desktop": true}`))
	if err != nil || !known || !info.Desktop {
		t.Errorf("an image with a desktop: %+v, %v, %v", info, known, err)
	}
	info, known, err = vm.ReadImageInfo(write(t, `{"desktop": false}`))
	if err != nil || !known || info.Desktop {
		t.Errorf("an image without one: %+v, %v, %v", info, known, err)
	}
	if _, known, err := vm.ReadImageInfo(t.TempDir()); err != nil || known {
		t.Errorf("an image that does not say: %v, %v", known, err)
	}
	if _, _, err := vm.ReadImageInfo(write(t, `{desktop`)); err == nil {
		t.Error("a file that is not JSON was read")
	}
}
