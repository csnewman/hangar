//go:build !darwin && krun

package main

// chooseVulkan and canZink are the Mac's: on Linux virglrenderer finds the
// host's Vulkan driver itself, and draws the guest's OpenGL with the host's
// own (virgl).
func chooseVulkan() string { return "" }

func canZink() bool { return false }
