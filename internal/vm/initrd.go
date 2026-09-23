package vm

import (
	"fmt"
	"io"
	"os"
	"path/filepath"
)

// bootInitrd writes the initramfs an environment boots: its image's, with
// the worker's agent appended at /hangar/hangar-agent, where the image's
// init installs it into the root. It returns the image's own initramfs when
// the worker has no agent to supply.
//
// The kernel unpacks an initramfs made of several archives one after
// another, compressed or not, so appending an uncompressed archive to a
// compressed one needs nothing else changed.
func bootInitrd(imageInitrd, agent, dir string) (string, error) {
	if agent == "" {
		return imageInitrd, nil
	}
	bin, err := os.ReadFile(agent)
	if err != nil {
		return "", fmt.Errorf("reading the agent: %w", err)
	}
	base, err := os.Open(imageInitrd)
	if err != nil {
		return "", err
	}
	defer base.Close()

	path := filepath.Join(dir, "initrd.img")
	out, err := os.Create(path + ".new")
	if err != nil {
		return "", err
	}
	n, err := io.Copy(out, base)
	if err == nil {
		// Archives start on a four-byte boundary; the kernel skips the
		// zeros between them.
		_, err = out.Write(make([]byte, (4-n%4)%4))
	}
	if err == nil {
		err = writeCpio(out, []cpioEntry{
			{name: "hangar", mode: 0o040755},
			{name: "hangar/hangar-agent", mode: 0o100755, data: bin},
		})
	}
	if cerr := out.Close(); err == nil {
		err = cerr
	}
	if err != nil {
		os.Remove(path + ".new")
		return "", err
	}
	return path, os.Rename(path+".new", path)
}

type cpioEntry struct {
	name string
	mode uint32
	data []byte
}

// writeCpio writes entries as a "newc" cpio archive, the format the kernel
// unpacks an initramfs from.
func writeCpio(w io.Writer, entries []cpioEntry) error {
	pad := func(n int) []byte { return make([]byte, (4-n%4)%4) }
	write := func(ino int, e cpioEntry) error {
		name := e.name + "\x00"
		nlink := 1
		if e.mode&0o040000 != 0 {
			nlink = 2
		}
		hdr := fmt.Sprintf("070701%08X%08X%08X%08X%08X%08X%08X%08X%08X%08X%08X%08X%08X",
			ino, e.mode, 0, 0, nlink, 0, len(e.data), 0, 0, 0, 0, len(name), 0)
		for _, b := range [][]byte{[]byte(hdr), []byte(name), pad(len(hdr) + len(name)), e.data, pad(len(e.data))} {
			if _, err := w.Write(b); err != nil {
				return err
			}
		}
		return nil
	}
	for i, e := range entries {
		if err := write(i+1, e); err != nil {
			return err
		}
	}
	return write(len(entries)+1, cpioEntry{name: "TRAILER!!!"})
}
