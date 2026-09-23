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
