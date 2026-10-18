//go:build darwin && !cgo

package vm

// processTicks needs libproc, reached through cgo.
func processTicks(int) uint64 { return 0 }

// guestRAM needs libproc, reached through cgo.
func guestRAM(int) int64 { return 0 }
