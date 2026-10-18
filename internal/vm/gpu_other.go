//go:build !darwin

package vm

// macGPUName is macOS's: libkrun renders on the Mac's GPU alone.
func macGPUName() string { return "Apple GPU" }
