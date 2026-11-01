package ch_test

import (
	"slices"
	"testing"

	"github.com/csnewman/hangar/internal/ch"
)

func memoryArg(t *testing.T, c ch.Config) string {
	t.Helper()
	c.Kernel = "vmlinuz"
	c.Disks = []ch.Disk{{Path: "upper.ext4"}}
	c.ConsoleFile = "console.log"
	args, err := c.Args()
	if err != nil {
		t.Fatal(err)
	}
	i := slices.Index(args, "--memory")
	if i < 0 || i+1 >= len(args) {
		t.Fatalf("no --memory in %q", args)
	}
	return args[i+1]
}

func TestMemoryArg(t *testing.T) {
	cases := []struct {
		name     string
		mem, max int
		want     string
	}{
		{"fixed", 4096, 0, "size=4096M,shared=on"},
		{"no room above", 4096, 4096, "size=4096M,shared=on"},
		// A quarter from boot, the rest plugged into a region reaching the
		// ceiling.
		{"resizable", 8192, 16384,
			"size=2048M,shared=on,hotplug_method=virtio-mem,hotplug_size=14336M,hotplugged_size=6144M"},
		// At least 1 GiB from boot, and a machine no bigger has nothing
		// plugged.
		{"small", 2048, 9880,
			"size=1024M,shared=on,hotplug_method=virtio-mem,hotplug_size=8960M,hotplugged_size=1024M"},
		{"smaller than the floor", 512, 4096,
			"size=512M,shared=on,hotplug_method=virtio-mem,hotplug_size=3584M"},
		// Plugged in whole 2 MiB blocks, rounded up.
		{"odd size", 3001, 8192,
			"size=1024M,shared=on,hotplug_method=virtio-mem,hotplug_size=7168M,hotplugged_size=1978M"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := memoryArg(t, ch.Config{MemoryMB: c.mem, MemoryMaxMB: c.max}); got != c.want {
				t.Errorf("--memory %s, want %s", got, c.want)
			}
		})
	}
}

func TestMemoryFit(t *testing.T) {
	m := ch.Memory{Boot: 1024, Max: 9984}
	cases := []struct{ in, want int }{
		{4096, 4096},
		{4097, 4098},
		{512, 1024},
		{20000, 9984},
	}
	for _, c := range cases {
		if got := m.Fit(c.in); got != c.want {
			t.Errorf("Fit(%d) = %d, want %d", c.in, got, c.want)
		}
	}
	if fixed := (ch.Memory{Boot: 2048, Max: 2048}); fixed.Resizable() || fixed.Fit(4096) != 2048 {
		t.Errorf("a machine that cannot be resized fits %d", fixed.Fit(4096))
	}
}

func TestBootMemory(t *testing.T) {
	for mb, want := range map[int]int{512: 512, 1024: 1024, 2048: 1024, 6144: 1536, 6000: 1408, 16384: 4096} {
		if got := ch.BootMemoryMB(mb); got != want {
			t.Errorf("BootMemoryMB(%d) = %d, want %d", mb, got, want)
		}
	}
}

func TestHotplug(t *testing.T) {
	cases := []struct {
		mem, max               int
		boot, region, inRegion int
	}{
		{4096, 0, 4096, 0, 0},
		{4096, 4096, 4096, 0, 0},
		{8192, 16384, 2048, 14336, 6144},
		{3001, 8192, 1024, 7168, 1978},
	}
	for _, c := range cases {
		boot, region, inRegion := ch.Hotplug(c.mem, c.max)
		if boot != c.boot || region != c.region || inRegion != c.inRegion {
			t.Errorf("Hotplug(%d, %d) = %d, %d, %d, want %d, %d, %d",
				c.mem, c.max, boot, region, inRegion, c.boot, c.region, c.inRegion)
		}
	}
}
