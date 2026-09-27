package vm

import (
	"time"

	"github.com/csnewman/hangar/internal/ch"
)

// hugePageCheckEvery is how often the host's huge page counts are read, and
// hugePageWarnEvery the least time between two warnings about them.
const (
	hugePageCheckEvery = time.Minute
	hugePageWarnEvery  = 15 * time.Minute
)

// watchHugePages warns while the host backs shared memory, which guest
// memory is, with 4 KiB pages for want of free 2 MB blocks. Nothing fails
// when it does, and guests slow down with nothing else to show why.
func (r *Runtime) watchHugePages() {
	last, err := ch.ReadHugePageCounts()
	if err != nil {
		r.cfg.Log.Debug("huge page counts are unavailable", "err", err)
		return
	}
	var warned time.Time
	var fellBack, allocated uint64
	t := time.NewTicker(hugePageCheckEvery)
	defer t.Stop()
	for {
		select {
		case <-r.ctx.Done():
			return
		case <-t.C:
		}
		c, err := ch.ReadHugePageCounts()
		if err != nil {
			continue
		}
		fellBack += c.FellBack - last.FellBack
		allocated += c.Allocated - last.Allocated
		last = c
		if fellBack == 0 || time.Since(warned) < hugePageWarnEvery {
			continue
		}
		r.cfg.Log.Warn("the host had no free 2 MB blocks for some guest memory, which runs on 4 KiB pages and is slower; "+
			"raise vm.min_free_kbytes or vm.compaction_proactiveness, or give the page cache less",
			"fell_back_mib", fellBack*2, "huge_mib", allocated*2)
		warned, fellBack, allocated = time.Now(), 0, 0
	}
}
