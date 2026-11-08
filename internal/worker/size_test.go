package worker_test

import (
	"testing"

	"github.com/csnewman/hangar/internal/worker"
)

func TestParseSize(t *testing.T) {
	for in, want := range map[string]worker.Size{
		"0":       0,
		"1024":    1024,
		"4GiB":    4 << 30,
		"512 MiB": 512 << 20,
		"1TiB":    1 << 40,
	} {
		got, err := worker.ParseSize(in)
		if err != nil || got != want {
			t.Errorf("ParseSize(%q) = %d, %v; want %d", in, got, err, want)
		}
	}
	for _, bad := range []string{"", "4GB", "-1", "lots"} {
		if _, err := worker.ParseSize(bad); err == nil {
			t.Errorf("ParseSize(%q) succeeded", bad)
		}
	}
}
