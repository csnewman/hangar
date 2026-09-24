//go:build !linux

package main

import (
	"fmt"
	"os"
)

// initRoot is Linux's alone: the agent is init only in a Linux guest.
func initRoot() {
	fmt.Fprintln(os.Stderr, "hangar-agent: running as init is only possible on Linux")
	os.Exit(1)
}
