//go:build darwin && !cgo

package worker

// cpuTimes reports nothing: Mach's counters are read through cgo.
func cpuTimes() cpuSample { return cpuSample{} }
