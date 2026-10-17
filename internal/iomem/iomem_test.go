package iomem_test

import (
	"strings"
	"testing"

	"github.com/csnewman/hangar/internal/iomem"
)

// As an arm64 guest with virtio-mem has it, its device region beyond a
// hole above the RAM it booted with, part of it plugged in.
const arm64 = `08ff0000-08ffffff : GICD
09000000-09000fff : pl011@9000000
  09000000-09000fff : 9000000.pl011 pl011@9000000
10000000-2fffffff : pci@30000000
    10000000-1007ffff : virtio-pci-modern
  10000000-1007ffff : 0000:00:01.0
    10000000-1007ffff : virtio1
30000000-300fffff : PCI ECAM
40000000-7fffffff : System RAM
  40000000-40000fff : reserved
  40400000-4170ffff : Kernel code
100000000-32fffffff : virtio6
  100000000-13fffffff : System RAM (virtio_mem)
330000000-ffffeeffff : pci@30000000
`

func TestRAMSpan(t *testing.T) {
	cases := []struct {
		name       string
		in         string
		start, end uint64
	}{
		{"virtio-mem", arm64, 0x40000000, 0x330000000},
		{"fixed memory", "00000000-00000fff : Reserved\n00001000-0009fbff : System RAM\n00100000-bfffffff : System RAM\n100000000-23fffffff : System RAM\n",
			0x1000, 0x240000000},
	}
	for _, c := range cases {
		start, end, err := iomem.RAMSpan(strings.NewReader(c.in))
		if err != nil {
			t.Fatalf("%s: %v", c.name, err)
		}
		if start != c.start || end != c.end {
			t.Errorf("%s: %#x-%#x, want %#x-%#x", c.name, start, end, c.start, c.end)
		}
	}
	if _, _, err := iomem.RAMSpan(strings.NewReader("00000000-00000000 : System RAM\n")); err == nil {
		t.Error("addresses hidden from an unprivileged reader gave a span")
	}
}
