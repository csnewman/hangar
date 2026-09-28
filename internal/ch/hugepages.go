package ch

import (
	"fmt"
	"os"
	"strconv"
	"strings"
)

// shmemEnabled is the host's policy for transparent huge pages on shared
// memory.
const shmemEnabled = "/sys/kernel/mm/transparent_hugepage/shmem_enabled"

// CheckHugePages reports whether this host will give shared memory
// transparent huge pages.
//
// It matters because nothing fails without them. Cloud Hypervisor already
// asks -- it calls madvise(MADV_HUGEPAGE) on the guest's memory -- but that
// call is ignored while the host's policy is "never", which is the value
// distributions ship. The guest then runs on 4 KiB pages and boots in three
// to five times as long, with no error anywhere to explain it. A silent
// regression of that size is worth refusing to start for.
//
// The file reads as a list with the active value in brackets, for example
//
//	always within_size advise [never] deny force
//
// "advise" is what Hangar wants: it grants huge pages only to callers that
// ask, which is the monitor and nothing else on the host.
func CheckHugePages() error {
	b, err := os.ReadFile(shmemEnabled)
	if err != nil {
		if os.IsNotExist(err) {
			return fmt.Errorf("this kernel has no %s, so shared guest memory "+
				"cannot take huge pages; Hangar needs a kernel built with "+
				"CONFIG_TRANSPARENT_HUGEPAGE", shmemEnabled)
		}
		return fmt.Errorf("reading %s: %w", shmemEnabled, err)
	}

	mode := selected(string(b))
	switch mode {
	case "advise", "always", "within_size", "force":
		return nil
	case "":
		return fmt.Errorf("could not tell which mode is active in %s (%q)",
			shmemEnabled, strings.TrimSpace(string(b)))
	default:
		// "never" and "deny" both leave the madvise with no effect.
		return fmt.Errorf("shared memory cannot take huge pages: %s is %q.\n"+
			"Guest memory has to be shared for virtio-fs, so without this a guest "+
			"boots three to five times slower with nothing to show why.\n"+
			"Fix it for this boot with:\n"+
			"    echo advise | sudo tee %s\n"+
			"and permanently with a line in /etc/tmpfiles.d/hangar-thp.conf:\n"+
			"    w %s - - - - advise",
			shmemEnabled, mode, shmemEnabled, shmemEnabled)
	}
}

// selected returns the value in brackets, which is the active one.
func selected(s string) string {
	for _, f := range strings.Fields(s) {
		if strings.HasPrefix(f, "[") && strings.HasSuffix(f, "]") {
			return strings.Trim(f, "[]")
		}
	}
	return ""
}

// HugePageCounts are the host's running totals of shared memory it has
// asked to back with a huge page: those it could, and those it fell back to
// 4 KiB pages for because no 2 MB block was free.
type HugePageCounts struct {
	Allocated, FellBack uint64
}

// ReadHugePageCounts reads the host's HugePageCounts from /proc/vmstat,
// where shared memory counts as file memory.
//
// Cloud Hypervisor's guest memory is shared memory, and a guest's memory
// that falls back runs on 4 KiB pages at the second stage of translation:
// slower on any host, and under nested virtualisation hundreds of times
// slower to touch. It happens when the host's free memory is fragmented, and
// the balloon makes it recur: memory a guest frees is handed back, and taken
// again whenever the guest next uses it.
func ReadHugePageCounts() (HugePageCounts, error) {
	b, err := os.ReadFile("/proc/vmstat")
	if err != nil {
		return HugePageCounts{}, err
	}
	var c HugePageCounts
	var seen int
	for _, line := range strings.Split(string(b), "\n") {
		name, v, ok := strings.Cut(line, " ")
		var dst *uint64
		switch {
		case !ok:
			continue
		case name == "thp_file_alloc":
			dst = &c.Allocated
		case name == "thp_file_fallback":
			dst = &c.FellBack
		default:
			continue
		}
		n, err := strconv.ParseUint(strings.TrimSpace(v), 10, 64)
		if err != nil {
			return HugePageCounts{}, fmt.Errorf("parsing %s in /proc/vmstat: %w", name, err)
		}
		*dst = n
		seen++
	}
	if seen != 2 {
		return HugePageCounts{}, fmt.Errorf("/proc/vmstat has no thp_file_alloc and thp_file_fallback")
	}
	return c, nil
}

// FreeHugeBlocksMiB is how much of the host's free memory is in blocks at
// least as large as a transparent huge page, which is all a guest's memory
// can take huge pages from without the kernel first compacting.
func FreeHugeBlocksMiB() (int, error) {
	pmd, err := os.ReadFile("/sys/kernel/mm/transparent_hugepage/hpage_pmd_size")
	if err != nil {
		return 0, err
	}
	huge, err := strconv.Atoi(strings.TrimSpace(string(pmd)))
	if err != nil {
		return 0, err
	}
	order := 0
	for size := os.Getpagesize(); size < huge; size *= 2 {
		order++
	}
	b, err := os.ReadFile("/proc/buddyinfo")
	if err != nil {
		return 0, err
	}
	var bytes int64
	for _, line := range strings.Split(string(b), "\n") {
		// Node 0, zone   Normal  <free blocks of order 0> <order 1> ...
		f := strings.Fields(line)
		if len(f) < 4 {
			continue
		}
		for i, count := range f[4:] {
			if i < order {
				continue
			}
			n, err := strconv.ParseInt(count, 10, 64)
			if err != nil {
				return 0, fmt.Errorf("parsing /proc/buddyinfo: %w", err)
			}
			bytes += n << i * int64(os.Getpagesize())
		}
	}
	return int(bytes >> 20), nil
}

// CompactMemory has the kernel move pages to make free memory contiguous
// again, so that it can back a guest's memory with huge pages.
func CompactMemory() error {
	return os.WriteFile("/proc/sys/vm/compact_memory", []byte("1"), 0)
}
