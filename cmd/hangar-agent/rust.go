package main

import (
	"fmt"
	"os"
	"os/exec"
	"runtime"
	"time"
)

// serveRust runs the agent's Rust half (crates/agent), which serves what
// has moved over from this agent -- terminals, the desktop, the Code tab's
// service, the process list, the guest's ports, and handing idle memory back
// to the host -- for as long as the guest runs, starting it again if it
// stops. It runs as the agent does, as root
// and never killed for memory, since every session it holds dies with it.
func serveRust() {
	// The child is told when the thread that started it ends, so that
	// thread is this goroutine's for good.
	runtime.LockOSThread()
	for {
		cmd := exec.Command(rustAgentInRoot, "serve")
		cmd.Stdout, cmd.Stderr = os.Stderr, os.Stderr
		endsWithAgent(cmd)
		started := time.Now()
		err := cmd.Run()
		fmt.Fprintf(os.Stderr, "hangar-agent: %s serve stopped: %v\n", rustAgentInRoot, err)
		// One that fails as it starts is not started again at once.
		if time.Since(started) < 10*time.Second {
			time.Sleep(5 * time.Second)
		} else {
			time.Sleep(time.Second)
		}
	}
}
