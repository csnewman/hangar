// Package oomscore ranks the guest's processes for the kernel's OOM killer.
//
// When memory runs out the kernel kills the process with the highest
// badness: its share of memory, in thousandths, plus its oom_score_adj,
// from -1000 (never killed) to 1000. A process's adj is inherited by
// everything it starts. The agent runs at -1000, since every terminal and
// SSH session dies with it, so what it starts is given a score of its own
// before it runs: Wrap starts it by way of the agent, which sets the score
// and then becomes the program (Exec). A score set after the program
// started would miss whatever it had already started, a shell's rc files'
// daemons among them.
package oomscore

import (
	"fmt"
	"os"
	"os/exec"
	"strconv"
	"syscall"
)

const (
	// Service is the processes a browser's tabs run on, the Code service
	// and the desktop's VNC server: small, and losing one closes every
	// tab using it, so they are killed only when nothing else is left.
	Service = -900
	// User is whatever the user runs -- shells, SSH commands, the editors'
	// helpers, the language servers -- which the kernel ranks by size
	// alone.
	User = 0
)

// Arg is the argument that makes the agent Exec what Wrap gave it.
const Arg = "oom-score"

// Wrap makes cmd start at score: exe, the agent's own binary, runs with
// Arg in its place, sets its score and becomes cmd's program. cmd's
// arguments, environment, directory, user and terminal are its own. A cmd
// that cannot start is left alone, for Start to report.
func Wrap(exe string, cmd *exec.Cmd, score int) {
	if cmd.Err != nil {
		return
	}
	if _, err := os.Stat(cmd.Path); err != nil {
		return
	}
	cmd.Args = append([]string{exe, Arg, strconv.Itoa(score), cmd.Path}, cmd.Args...)
	cmd.Path = exe
}

// Exec is the agent's side of Wrap, given the arguments after Arg: the
// score, the program and its argv. It returns only if the program could
// not be run. A score that cannot be set is reported and the program run
// anyway, since a process ranked wrongly is better than none.
func Exec(args []string) error {
	if len(args) < 3 {
		return fmt.Errorf("%s wants a score, a program and its arguments", Arg)
	}
	if _, err := strconv.Atoi(args[0]); err != nil {
		return fmt.Errorf("%s: bad score %q", Arg, args[0])
	}
	if err := os.WriteFile("/proc/self/oom_score_adj", []byte(args[0]), 0); err != nil {
		fmt.Fprintf(os.Stderr, "hangar-agent: setting the OOM score: %v\n", err)
	}
	return syscall.Exec(args[1], args[2:], os.Environ())
}
