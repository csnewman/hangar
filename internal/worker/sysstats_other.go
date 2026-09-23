//go:build !linux

package worker

import "github.com/csnewman/hangar/internal/api"

// readSystem reports nothing off Linux: workers run environments on Linux
// alone, and a development build elsewhere has nothing worth measuring.
func readSystem(string) (*api.WorkerStats, cpuSample, bool) { return nil, cpuSample{}, false }
