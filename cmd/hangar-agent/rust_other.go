//go:build !linux

package main

import "os/exec"

// endsWithAgent does nothing: only Linux stops a child when its parent
// ends, and only a Linux guest runs the agent.
func endsWithAgent(*exec.Cmd) {}
