package main

import (
	"bufio"
	"errors"
	"fmt"
	"os"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// A guest with swap hands back the memory its programs leave idle as Meta's
// Senpai does (TMO): the agent asks the kernel again and again to reclaim a
// little of the guest's memory, and the kernel takes what was used least
// recently, page by page -- a program's memory into zswap's compressed pool
// and on to the swap disk, its cache of files dropped. Free page reporting
// hands what that frees to the host.
//
// Asking costs the guest nothing while what is taken stays unused. When it
// starts to cost, the guest's processes wait on memory to be brought back,
// which the kernel's pressure stall information counts, and the agent asks
// for less, then for nothing until the waiting stops.
//
// Pages read back from a fast disk barely stall anything, so the pressure
// alone lets the agent go on taking a guest's working set, which comes
// straight back: an idle guest re-reading its files at megabytes a second,
// its free memory scattered in pieces free page reporting cannot hand to
// the host. The kernel counts what is read back (its refaults), some of it
// long after it was taken. Once what was read back over the last minute or
// so is a share of what was taken over it, what is left is in use, and the
// agent waits before asking again, twice as long each time it finds the
// same, and half as long each time it does not.
const (
	// offloadEvery is how often the agent asks.
	offloadEvery = 5 * time.Second
	// offloadStep is how much of the guest's memory it asks for each time
	// while nothing waits on memory: a program idle for a minute or two is
	// out of memory, whatever its size.
	offloadStep = 0.01
	// pressureTarget is the share of the time some process may spend
	// waiting on memory, at which the agent stops asking.
	pressureTarget = 0.001
	// refaultShare is the share of what was taken that may be read back
	// before the agent stops asking.
	refaultShare = 0.25
	// refaultDecay is how much of what was taken and read back carries
	// over from one round to the next: about a minute's worth counts.
	refaultDecay = 0.92
	// maxHold is the longest the agent then waits before asking again.
	maxHold = 5 * time.Minute
)

const (
	memoryReclaim  = "/sys/fs/cgroup/memory.reclaim"
	memoryPressure = "/proc/pressure/memory"
)

// offload asks the kernel to reclaim, every offloadEvery, for as long as the
// guest runs. A guest without swap is left to DAMON (widenReclaim), which
// reclaims only its cache.
func offload() {
	if _, err := os.Stat(memoryReclaim); err != nil {
		return
	}
	prev, err := memoryStall()
	if err != nil {
		fmt.Fprintf(os.Stderr, "hangar-agent: offload: %v\n", err)
		return
	}
	page := int64(os.Getpagesize())
	taken, back := reclaimCounts()
	// tookRecently and backRecently are what was taken and read back over
	// the last minute or so, in bytes.
	var tookRecently, backRecently float64
	// hold is how many rounds the agent last waited, and wait how many it
	// has still to wait.
	var hold, wait int
	t := time.NewTicker(offloadEvery)
	defer t.Stop()
	for range t.C {
		stall, err := memoryStall()
		if err != nil {
			continue
		}
		pressure := float64(stall-prev) / float64(offloadEvery.Microseconds())
		prev = stall
		// What the last round took, and what this one read back.
		took, readBack := taken, back
		taken, back = reclaimCounts()
		took, readBack = (taken-took)*page, (back-readBack)*page
		tookRecently = tookRecently*refaultDecay + float64(took)
		backRecently = backRecently*refaultDecay + float64(readBack)
		switch {
		case took > 0 && backRecently > tookRecently*refaultShare:
			hold = min(max(2*hold, 1), int(maxHold/offloadEvery))
			wait = hold
		case took > 0:
			hold /= 2
		}
		if wait > 0 {
			wait--
			continue
		}
		if pressure >= pressureTarget {
			continue
		}
		mem, swap := memoryAndSwap()
		if swap == 0 {
			continue
		}
		n := int64(float64(mem) * offloadStep * (1 - pressure/pressureTarget))
		if n < 1<<20 {
			continue
		}
		// EAGAIN is that it took less than asked: everything left is in use.
		if err := writeReclaim(n); err != nil && !errors.Is(err, syscall.EAGAIN) {
			fmt.Fprintf(os.Stderr, "hangar-agent: offload: %v\n", err)
		}
	}
}

func writeReclaim(bytes int64) error {
	f, err := os.OpenFile(memoryReclaim, os.O_WRONLY, 0)
	if err != nil {
		return err
	}
	_, err = f.WriteString(strconv.FormatInt(bytes, 10))
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	return err
}

// reclaimCounts are the pages the agent's reclaim has taken since boot and
// the pages read back after reclaim took them (/proc/vmstat).
func reclaimCounts() (taken, back int64) {
	b, err := os.ReadFile("/proc/vmstat")
	if err != nil {
		return 0, 0
	}
	for _, line := range strings.Split(string(b), "\n") {
		key, v, ok := strings.Cut(line, " ")
		if !ok {
			continue
		}
		n, _ := strconv.ParseInt(v, 10, 64)
		switch key {
		case "pgsteal_proactive":
			taken += n
		case "workingset_refault_anon", "workingset_refault_file":
			back += n
		}
	}
	return taken, back
}

// memoryStall is how long, in microseconds since boot, some process has
// waited on memory: the "some" line's total.
func memoryStall() (uint64, error) {
	b, err := os.ReadFile(memoryPressure)
	if err != nil {
		return 0, err
	}
	for _, line := range strings.Split(string(b), "\n") {
		if !strings.HasPrefix(line, "some ") {
			continue
		}
		for _, f := range strings.Fields(line) {
			if v, ok := strings.CutPrefix(f, "total="); ok {
				return strconv.ParseUint(v, 10, 64)
			}
		}
	}
	return 0, fmt.Errorf("no stall total in %s", memoryPressure)
}

// memoryAndSwap are the guest's memory and its swap, in bytes. Its memory
// changes as virtio-mem plugs and unplugs it.
func memoryAndSwap() (mem, swap int64) {
	f, err := os.Open("/proc/meminfo")
	if err != nil {
		return 0, 0
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		key, rest, ok := strings.Cut(sc.Text(), ":")
		if !ok {
			continue
		}
		fields := strings.Fields(rest)
		if len(fields) == 0 {
			continue
		}
		kib, _ := strconv.ParseInt(fields[0], 10, 64)
		switch key {
		case "MemTotal":
			mem = kib << 10
		case "SwapTotal":
			swap = kib << 10
		}
	}
	return mem, swap
}
