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
	t := time.NewTicker(offloadEvery)
	defer t.Stop()
	for range t.C {
		stall, err := memoryStall()
		if err != nil {
			continue
		}
		pressure := float64(stall-prev) / float64(offloadEvery.Microseconds())
		prev = stall
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
