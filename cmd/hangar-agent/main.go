// Command hangar-agent runs as PID-something inside an environment and gives
// the host a control channel.
//
// It dials out over vsock rather than listening. The host therefore needs no
// address for this guest and nothing waits for the guest's network, so an
// environment is reachable while it is still booting, and stays reachable if
// its networking is broken or deliberately cut off.
//
// The binary is static and depends on nothing in the image, because it is
// supplied by the node rather than built into the base: an environment built
// from an arbitrary Dockerfile still gets the same agent.
package main

import (
	"bufio"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/csnewman/hangar/internal/agent"
	"github.com/csnewman/hangar/internal/vsock"
)

func main() {
	// In the initramfs the kernel starts the agent as init.
	if os.Getpid() == 1 {
		initRoot()
		return
	}
	port := flag.Uint("port", uint(agent.Port), "vsock port on the host")
	retry := flag.Duration("retry", 500*time.Millisecond, "delay between connection attempts")
	flag.Parse()

	go serveTerminals()
	go serveEditor()
	go serveDesktop()
	go serveCode()
	go serveProfile()
	go serveProcs()
	run(uint32(*port), *retry)
}

// run keeps the agent connected to the host for as long as the guest runs.
//
// The host end is not permanent. It goes away when the environment is
// suspended and comes back, as a different process, when it is resumed --
// possibly after the host itself has restarted. From in here that is a
// connection that ends and a host that answers again later, so the agent
// redials rather than exiting. The monitor resets every connection the guest
// had open when it restores a snapshot, which is what ends the old one.
func run(port uint32, retry time.Duration) {
	for {
		conn, err := vsock.Dial(vsock.CIDHost, port)
		if err != nil {
			// Expected while the host is not listening: during early boot,
			// which is when the agent starts, and while an environment is
			// being restored.
			time.Sleep(retry)
			continue
		}
		if err := sendHello(conn); err != nil {
			fmt.Fprintf(os.Stderr, "hangar-agent: %v\n", err)
		} else {
			serve(conn)
		}
		conn.Close()
		time.Sleep(retry)
	}
}

func sendHello(conn *os.File) error {
	h := agent.Hello{
		Kind:       agent.KindHello,
		Version:    agent.ProtocolVersion,
		Hostname:   hostname(),
		Kernel:     kernelRelease(),
		BootMicros: uptimeMicros(),
	}
	b, err := json.Marshal(h)
	if err != nil {
		return err
	}
	_, err = conn.Write(append(b, '\n'))
	return err
}

// serve answers requests until the host hangs up.
//
// Each request is handled on its own, and its reply written when it is ready,
// so a command that runs for minutes does not hold up a ping sent after it.
// The host matches replies to requests by ID.
func serve(conn *os.File) {
	var wmu sync.Mutex
	reply := func(resp agent.Response) {
		b, err := json.Marshal(resp)
		if err != nil {
			return
		}
		wmu.Lock()
		defer wmu.Unlock()
		// A failed write means the host went away, which the read loop sees
		// too; see run.
		_, _ = conn.Write(append(b, '\n'))
	}

	br := bufio.NewReader(conn)
	for {
		line, err := br.ReadBytes('\n')
		if err != nil {
			// The host went away. See run.
			return
		}
		var req agent.Request
		if err := json.Unmarshal(line, &req); err != nil {
			continue
		}
		go func() { reply(handle(req)) }()
	}
}

func handle(req agent.Request) agent.Response {
	resp := agent.Response{ID: req.ID}
	switch req.Kind {
	case agent.KindPing:
		return resp
	case agent.KindInfo:
		resp.Info = fmt.Sprintf("%s %s up %dus", hostname(), kernelRelease(), uptimeMicros())
		return resp
	case agent.KindExec:
		if len(req.Cmd) == 0 {
			resp.Err = "exec with no command"
			return resp
		}
		return execute(req.ID, req.Cmd)
	case agent.KindSetClock:
		if err := setClock(req.UnixNanos); err != nil {
			resp.Err = err.Error()
		}
		return resp
	default:
		resp.Err = "unknown request kind " + strconv.Quote(req.Kind)
		return resp
	}
}

func execute(id uint64, argv []string) agent.Response {
	resp := agent.Response{ID: id}

	cmd := exec.Command(argv[0], argv[1:]...)
	var stdout, stderr strings.Builder
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	// The reply is due when the command exits, not when every process that
	// inherited its output has closed it. A command that starts something
	// long-lived in the background -- a desktop, a server -- leaves that
	// process holding the pipes, and would otherwise never be answered. What
	// the command itself wrote is collected; anything written after this is
	// not.
	cmd.WaitDelay = outputGrace

	err := cmd.Run()
	if errors.Is(err, exec.ErrWaitDelay) {
		// The command exited, successfully, and left something holding its
		// output. That is the case above, not a failure.
		err = nil
	}
	resp.Stdout = stdout.String()
	resp.Stderr = stderr.String()

	switch e := err.(type) {
	case nil:
		resp.Code = 0
	case *exec.ExitError:
		// The command ran and failed. That is a result, not an error: the
		// caller wants the exit status, not a transport failure.
		resp.Code = e.ExitCode()
	default:
		resp.Err = err.Error()
	}
	return resp
}

// outputGrace is how long a command's output is still collected after the
// command exits, for processes it left behind to finish writing what they
// were writing.
const outputGrace = 500 * time.Millisecond

// setClock sets the wall clock. The host sends its own time whenever it
// connects; see agent.Session.SetClock.
func setClock(unixNanos int64) error {
	tv := syscall.NsecToTimeval(unixNanos)
	if err := syscall.Settimeofday(&tv); err != nil {
		return fmt.Errorf("settimeofday: %w", err)
	}
	return nil
}

func hostname() string {
	h, err := os.Hostname()
	if err != nil {
		return "unknown"
	}
	return h
}

// kernelRelease reads the running kernel version.
//
// This comes from /proc rather than uname(2) because the syscall package's
// Utsname is Linux-only and its Release field is int8 on one architecture and
// uint8 on another. A file read has neither problem, and the agent already
// reads /proc/uptime.
func kernelRelease() string {
	b, err := os.ReadFile("/proc/sys/kernel/osrelease")
	if err != nil {
		return "unknown"
	}
	return strings.TrimSpace(string(b))
}

// uptimeMicros reports how long the guest has been running, from the kernel's
// own clock. Measuring here rather than on the host excludes monitor startup
// and host scheduling, so it answers "how long did the guest take to become
// reachable" rather than "how long did the whole run take".
func uptimeMicros() int64 {
	b, err := os.ReadFile("/proc/uptime")
	if err != nil {
		return 0
	}
	first, _, _ := strings.Cut(strings.TrimSpace(string(b)), " ")
	secs, err := strconv.ParseFloat(first, 64)
	if err != nil {
		return 0
	}
	return int64(secs * 1e6)
}
