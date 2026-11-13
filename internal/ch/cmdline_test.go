package ch_test

import (
	"strings"
	"testing"

	"github.com/csnewman/hangar/internal/ch"
)

// DAMON's reclaim is set up on every guest and left off: the agent turns
// it on while its environment asks for reclaim.
func TestReclaimCmdline(t *testing.T) {
	cl := (&ch.Config{ConsoleTTY: "hvc0", ExtraCmdline: "quiet"}).Cmdline()
	for _, want := range []string{"damon_reclaim.min_age=300000000", "damon_reclaim.skip_anon=Y"} {
		if !strings.Contains(cl, " "+want+" ") {
			t.Errorf("%q is missing: %s", want, cl)
		}
	}
	if strings.Contains(cl, "damon_reclaim.enabled=Y") {
		t.Errorf("DAMON's reclaim is on from boot: %s", cl)
	}
	if !strings.HasSuffix(cl, " quiet") {
		t.Errorf("the extra command line is not last: %s", cl)
	}
}
