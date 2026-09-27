//go:build !linux && !darwin

package worker

import (
	"fmt"
	"runtime"

	"github.com/csnewman/hangar/internal/api"
)

func machineCapacity() (api.Resources, error) {
	return api.Resources{}, fmt.Errorf("measuring memory is not supported on %s", runtime.GOOS)
}

// swapMiB is 0: environments run only on Linux, so there is nothing to
// overcommit here.
func swapMiB() int { return 0 }
