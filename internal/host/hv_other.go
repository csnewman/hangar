//go:build !darwin

package host

// hypervisorFramework is macOS's alone.
func hypervisorFramework() bool { return false }
