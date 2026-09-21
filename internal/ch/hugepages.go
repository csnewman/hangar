package ch

import (
	"fmt"
	"os"
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
