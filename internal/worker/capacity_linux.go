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
	f, err := os.Open("/proc/meminfo")
	if err != nil {
		return api.Resources{}, err
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		rest, ok := strings.CutPrefix(sc.Text(), "MemTotal:")
		if !ok {
			continue
		}
		kib, err := strconv.Atoi(strings.TrimSpace(strings.TrimSuffix(strings.TrimSpace(rest), "kB")))
		if err != nil {
			return api.Resources{}, fmt.Errorf("parsing MemTotal: %w", err)
		}
		return api.Resources{CPUs: runtime.NumCPU(), MemoryMiB: kib / 1024}, nil
	}
	if err := sc.Err(); err != nil {
		return api.Resources{}, err
	}
	return api.Resources{}, fmt.Errorf("no MemTotal in /proc/meminfo")
}
