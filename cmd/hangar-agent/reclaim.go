package main

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"

	"github.com/csnewman/hangar/internal/iomem"
)

// damonReclaim is where DAMON's reclaim of pages the guest has not touched
// for a while is set. The kernel's command line turns it on (ch.Config).
const damonReclaim = "/sys/module/damon_reclaim/parameters"

// widenReclaim has DAMON watch all of the guest's RAM. Left to itself it
// watches the largest block the guest booted with, which leaves out what
// virtio-mem plugs in: most of a resizable machine's memory, and the whole
// of what it grows into.
func widenReclaim() {
	if _, err := os.Stat(damonReclaim); err != nil {
		return
	}
	f, err := os.Open("/proc/iomem")
	if err != nil {
		fmt.Fprintf(os.Stderr, "hangar-agent: reclaim: %v\n", err)
		return
	}
	start, end, err := iomem.RAMSpan(f)
	f.Close()
	if err != nil {
		fmt.Fprintf(os.Stderr, "hangar-agent: reclaim: %v\n", err)
		return
	}
	set := func(name, value string) error {
		return os.WriteFile(filepath.Join(damonReclaim, name), []byte(value), 0o600)
	}
	// The region is read only when the inputs are committed, and it is
	// committed whole, so both ends are written first.
	if err := errors.Join(
		set("monitor_region_start", strconv.FormatUint(start, 10)),
		set("monitor_region_end", strconv.FormatUint(end, 10)),
		set("commit_inputs", "Y"),
	); err != nil {
		fmt.Fprintf(os.Stderr, "hangar-agent: reclaim: %v\n", err)
	}
}
