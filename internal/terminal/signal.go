package terminal

import (
	"os"

	"golang.org/x/sys/unix"
)

// signalForeground tells whatever program has the terminal that its window
// changed, which a full-screen program answers by drawing itself again.
func signalForeground(ptmx *os.File) {
	pgrp, err := unix.IoctlGetInt(int(ptmx.Fd()), unix.TIOCGPGRP)
	if err == nil && pgrp > 0 {
		_ = unix.Kill(-pgrp, unix.SIGWINCH)
	}
}

// hangupGroup hangs up on a shell and everything it started, as closing a
// terminal window does. The shell leads a session of its own, so its process
// ID is its group's.
func hangupGroup(pid int) {
	_ = unix.Kill(-pid, unix.SIGHUP)
}
