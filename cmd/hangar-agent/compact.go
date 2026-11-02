package main

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"
)

// Free page reporting hands the host only whole blocks of free memory (2 MiB
// with 4 KiB pages: the page reporting order), so memory the guest frees a
// page here and a page there stays the host's to keep: hundreds of MiB in an
// idle guest, what its reclaim and its programs left scattered. Compacting
// moves the pages in use together and joins what is free into blocks, which
// are then reported. The kernel compacts by itself only when most of its free
// memory is scattered, and a guest has most of its free memory in whole
// blocks however much is scattered, so the agent compacts it.
//
// Compacting a whole guest takes a second or two of a vCPU nested, far less
// on real hardware, and some of what is scattered cannot be joined (pages
// the kernel cannot move hold their blocks), so the agent compacts only when
// what is scattered has grown by compactGrowth since it last did.
const (
	// compactEvery is how often the agent looks.
	compactEvery = time.Minute
	// compactGrowth is how much scattered free memory may build up before
	// the agent compacts.
	compactGrowth = 64 << 20
)

const (
	compactMemory  = "/proc/sys/vm/compact_memory"
	reportingOrder = "/sys/module/page_reporting/parameters/page_reporting_order"
)

// compact compacts the guest's memory, every compactEvery, when its
// scattered free memory has grown by compactGrowth.
func compact() {
	b, err := os.ReadFile(reportingOrder)
	if err != nil {
		// No free page reporting: nothing the guest frees reaches the host.
		return
	}
	order, err := strconv.Atoi(strings.TrimSpace(string(b)))
	if err != nil {
		fmt.Fprintf(os.Stderr, "hangar-agent: compact: %s: %v\n", reportingOrder, err)
		return
	}
	page := int64(os.Getpagesize())
	// after is what was scattered once the agent last compacted: what it
	// cannot join, and the mark it compacts again above.
	var after int64
	t := time.NewTicker(compactEvery)
	defer t.Stop()
	for range t.C {
		if scatteredFree(order, page) < after+compactGrowth {
			continue
		}
		if err := os.WriteFile(compactMemory, []byte("1"), 0); err != nil {
			fmt.Fprintf(os.Stderr, "hangar-agent: compact: %v\n", err)
			return
		}
		after = scatteredFree(order, page)
	}
}

// scatteredFree is the guest's free memory in blocks smaller than 2^order
// pages (/proc/buddyinfo), in bytes.
func scatteredFree(order int, page int64) int64 {
	b, err := os.ReadFile("/proc/buddyinfo")
	if err != nil {
		return 0
	}
	var total int64
	for _, line := range strings.Split(string(b), "\n") {
		// "Node 0, zone   Normal  <free blocks of order 0> <of order 1> ..."
		fields := strings.Fields(line)
		if len(fields) < 4 {
			continue
		}
		for o, f := range fields[4:] {
			if o >= order {
				break
			}
			n, _ := strconv.ParseInt(f, 10, 64)
			total += n << o * page
		}
	}
	return total
}
