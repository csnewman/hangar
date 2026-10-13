package main

import (
	"os"
	"os/exec"
	"path/filepath"

	"github.com/csnewman/hangar/internal/oomscore"
)

// oomDropIn is the agent's unit's OOM settings, which init installs with
// the agent, so they come with the agent the worker supplies whatever the
// image's own unit says.
//
// The agent is never killed: every terminal and SSH session dies with it.
// What it starts is given a score of its own (ranked).
//
// OOMPolicy=continue keeps the unit running when the kernel kills one of
// its processes. Everything the agent starts is in its unit, and systemd's
// default for a service, stop, would end the lot -- every terminal, the
// Code service, the editors -- for one build that ran out of memory.
const oomDropIn = `[Service]
OOMScoreAdjust=-1000
OOMPolicy=continue
`

// oomDropInPath is in the system's unit search path, beside the agent in
// /usr/local, so the image's unit and /etc are left to it and the user.
const oomDropInPath = "/usr/local/lib/systemd/system/hangar-agent.service.d/oom.conf"

// installOOMDropIn writes oomDropIn into the root at newRoot.
func installOOMDropIn(newRoot string) error {
	p := filepath.Join(newRoot, oomDropInPath)
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		return err
	}
	return os.WriteFile(p, []byte(oomDropIn), 0o644)
}

// ranked is how the agent's servers start their processes: at score
// (internal/oomscore), by way of the agent's own binary, which in the new
// process /proc/self/exe still is.
func ranked(score int) func(*exec.Cmd) {
	return func(cmd *exec.Cmd) { oomscore.Wrap("/proc/self/exe", cmd, score) }
}
