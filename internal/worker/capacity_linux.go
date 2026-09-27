package worker

import (
	"bufio"
	"fmt"
	"os"
	"runtime"
	"strconv"
	"strings"

	"github.com/csnewman/hangar/internal/api"
)

func machineCapacity() (api.Resources, error) {
	kib, err := meminfo("MemTotal")
	if err != nil {
		return api.Resources{}, err
	}
	return api.Resources{CPUs: runtime.NumCPU(), MemoryMiB: kib / 1024}, nil
}

// swapMiB is how much swap the machine has, or 0 when that cannot be read.
func swapMiB() int {
	kib, err := meminfo("SwapTotal")
	if err != nil {
		return 0
	}
	return kib / 1024
}

// meminfo reads one field of /proc/meminfo, in KiB.
func meminfo(field string) (int, error) {
	f, err := os.Open("/proc/meminfo")
	if err != nil {
		return 0, err
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		rest, ok := strings.CutPrefix(sc.Text(), field+":")
		if !ok {
			continue
		}
		kib, err := strconv.Atoi(strings.TrimSpace(strings.TrimSuffix(strings.TrimSpace(rest), "kB")))
		if err != nil {
			return 0, fmt.Errorf("parsing %s: %w", field, err)
		}
		return kib, nil
	}
	if err := sc.Err(); err != nil {
		return 0, err
	}
	return 0, fmt.Errorf("no %s in /proc/meminfo", field)
}
