package vm

import "golang.org/x/sys/unix"

// macGPUName is the Mac's chip, whose GPU renders for its guests.
func macGPUName() string {
	if s, err := unix.Sysctl("machdep.cpu.brand_string"); err == nil && s != "" {
		return s
	}
	return "Apple GPU"
}
