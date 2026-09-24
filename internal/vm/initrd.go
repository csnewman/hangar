package vm

import (
	"fmt"
	"io"
	"os"
)

// WriteInitrd writes the initramfs an environment boots: the agent, as its
// init. Nothing else is needed: the agent assembles the root from the base
// over virtiofs and the writable disk, installs itself there, and hands over
// to the image's own init (cmd/hangar-agent/init.go). So an image is a root
// filesystem and nothing else, and every environment runs the agent of the
// worker that boots it.
func WriteInitrd(agent, path string) error {
	bin, err := os.ReadFile(agent)
	if err != nil {
		return fmt.Errorf("reading the agent: %w", err)
	}
	out, err := os.Create(path + ".new")
	if err != nil {
		return err
	}
	err = writeCpio(out, []cpioEntry{{name: "init", mode: 0o100755, data: bin}})
	if cerr := out.Close(); err == nil {
		err = cerr
	}
	if err != nil {
		os.Remove(path + ".new")
		return err
	}
	return os.Rename(path+".new", path)
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
