package ch

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"sync"
	"time"
)

// launch starts a backend and waits for it to create the socket the monitor
// will connect to.
//
// A backend that exits instead is reported at once, with what it wrote to
// stderr, rather than as a socket that never appeared. The usual cause is a
// binary that does not understand its arguments -- an older build earlier on
// PATH, say -- and its own message says so where a timeout would not.
func launch(cmd *exec.Cmd, name, socket string, timeout time.Duration, verbose bool) error {
	tail := &tailBuffer{max: 4096}
	if verbose {
		cmd.Stdout = os.Stderr
		cmd.Stderr = os.Stderr
	} else {
		cmd.Stderr = tail
	}
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("starting %s: %w", name, err)
	}
	exited := make(chan error, 1)
	go func() { exited <- cmd.Wait() }()

	deadline := time.After(timeout)
	for {
		if _, err := os.Stat(socket); err == nil {
			return nil
		}
		select {
		case err := <-exited:
			msg := strings.TrimSpace(tail.String())
			if msg == "" {
				return fmt.Errorf("%s exited before creating %s: %v", name, socket, err)
			}
			return fmt.Errorf("%s exited before creating %s: %v:\n%s", name, socket, err, msg)
		case <-deadline:
			_ = cmd.Process.Kill()
			return fmt.Errorf("%s did not create %s within %s", name, socket, timeout)
		case <-time.After(20 * time.Millisecond):
		}
	}
}

// tailBuffer keeps the last max bytes written to it.
type tailBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
	max int
}

func (t *tailBuffer) Write(p []byte) (int, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.buf.Write(p)
	if over := t.buf.Len() - t.max; over > 0 {
		t.buf.Next(over)
	}
	return len(p), nil
}

func (t *tailBuffer) String() string {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.buf.String()
}
