package host

import "golang.org/x/sys/unix"

// hypervisorFramework is whether macOS offers its hypervisor here.
func hypervisorFramework() bool {
	v, err := unix.SysctlUint32("kern.hv_support")
	return err == nil && v == 1
}
