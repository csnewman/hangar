package vm

import (
	"errors"
	"os"
	"path/filepath"

	"github.com/csnewman/hangar/internal/api"
)

// swapMiB is the swap an environment's spec asks for, or this worker's
// default for one that does not say: Config.Swap times its memory.
func (r *Runtime) swapMiB(s api.Spec) int {
	if s.SwapMiB != nil {
		return *s.SwapMiB
	}
	return int(float64(s.MemoryMiB) * r.cfg.Swap)
}

// swapFile is the machine's swap disk.
func (c InstanceConfig) swapFile() string { return filepath.Join(c.Dir, "swap.img") }

// makeSwap gives a machine booting from its disks an empty swap disk of
// SwapMiB, or takes away the one it had when SwapMiB is 0: what a guest
// swapped out is of no use to the next boot. The file is sparse, and the
// guest writes the swap header itself, for its own page size.
func (c InstanceConfig) makeSwap() error {
	path := c.swapFile()
	if c.SwapMiB <= 0 {
		if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
			return err
		}
		return nil
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o600)
	if err != nil {
		return err
	}
	err = f.Truncate(int64(c.SwapMiB) << 20)
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	return err
}

// dropSwap deletes the swap disk of a machine that has stopped, which the
// next boot makes afresh, unless it is suspended: the guest it resumes
// wants back what it swapped out.
func (c InstanceConfig) dropSwap() {
	if c.suspended() == nil {
		os.Remove(c.swapFile())
	}
}
