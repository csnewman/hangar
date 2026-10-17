// Package iomem reads a guest's physical memory map, /proc/iomem.
package iomem

import (
	"bufio"
	"errors"
	"io"
	"regexp"
	"strconv"
	"strings"
)

// virtioMem is the name the kernel gives virtio-mem's device region, the
// only virtio device a Hangar guest has at the top of the map: the others
// are beneath the PCI bridge.
var virtioMem = regexp.MustCompile(`^virtio[0-9]+$`)

// RAMSpan is the span of physical addresses the guest's RAM can be in,
// [start, end): from the lowest RAM it booted with to the end of what
// virtio-mem can plug in, which may be well beyond the RAM it has. The
// span has holes, which are not RAM.
func RAMSpan(r io.Reader) (start, end uint64, err error) {
	sc := bufio.NewScanner(r)
	found := false
	for sc.Scan() {
		line := sc.Text()
		// Nested resources are indented; only the top level is wanted.
		if line == "" || line[0] == ' ' {
			continue
		}
		span, name, ok := strings.Cut(line, " : ")
		if !ok || name != "System RAM" && !virtioMem.MatchString(name) {
			continue
		}
		lo, hi, ok := strings.Cut(span, "-")
		if !ok {
			continue
		}
		a, err1 := strconv.ParseUint(lo, 16, 64)
		b, err2 := strconv.ParseUint(hi, 16, 64)
		if err1 != nil || err2 != nil {
			continue
		}
		if !found || a < start {
			start = a
		}
		end = max(end, b+1)
		found = true
	}
	if err := sc.Err(); err != nil {
		return 0, 0, err
	}
	if !found {
		// Unprivileged readers are shown every address as zero.
		return 0, 0, errors.New("no RAM in the memory map")
	}
	if start == 0 && end == 1 {
		return 0, 0, errors.New("the memory map's addresses are hidden: it must be read as root")
	}
	return start, end, nil
}
