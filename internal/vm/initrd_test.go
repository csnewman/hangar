package vm_test

import (
	"bytes"
	"os"
	"path/filepath"
	"strconv"
	"testing"

	"github.com/csnewman/hangar/internal/vm"
)

// files reads a newc cpio archive: each file's name and contents, the
// trailer left out.
func files(t *testing.T, b []byte) map[string]string {
	t.Helper()
	out := map[string]string{}
	field := func(hdr []byte, i int) int {
		n, err := strconv.ParseUint(string(hdr[6+8*i:6+8*i+8]), 16, 32)
		if err != nil {
			t.Fatalf("header field %d: %v", i, err)
		}
		return int(n)
	}
	up := func(n int) int { return (n + 3) &^ 3 }
	for len(b) >= 110 {
		hdr := b[:110]
		if !bytes.HasPrefix(hdr, []byte("070701")) {
			t.Fatalf("not a newc header: %q", hdr[:6])
		}
		size, namesize := field(hdr, 6), field(hdr, 11)
		name := string(b[110 : 110+namesize-1])
		data := up(110 + namesize)
		if name == "TRAILER!!!" {
			return out
		}
		out[name] = string(b[data : data+size])
		b = b[up(data+size):]
	}
	t.Fatal("no trailer")
	return nil
}

func TestInitrdHoldsTheAgentAsInit(t *testing.T) {
	dir := t.TempDir()
	agent := filepath.Join(dir, "hangar-agent")
	initrd := filepath.Join(dir, "initrd")
	if err := vm.WriteInitrd(agent, initrd); err == nil {
		t.Fatal("an initramfs without the agent")
	}
	if err := os.WriteFile(agent, []byte("the agent"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := vm.WriteInitrd(agent, initrd); err != nil {
		t.Fatal(err)
	}
	b, err := os.ReadFile(initrd)
	if err != nil {
		t.Fatal(err)
	}
	if got := files(t, b); got["init"] != "the agent" || len(got) != 1 {
		t.Fatalf("the initramfs holds %v", got)
	}
}
