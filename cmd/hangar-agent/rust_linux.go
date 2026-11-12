package main

import (
	"os/exec"
	"syscall"
)

// endsWithAgent has cmd stopped when the agent's thread that started it
// ends.
func endsWithAgent(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Pdeathsig: syscall.SIGTERM}
}
