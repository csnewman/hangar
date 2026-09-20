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
	"flag"
	"fmt"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"time"

	"github.com/csnewman/hangar/internal/agent"
	"github.com/csnewman/hangar/internal/vsock"
)

func main() {
	port := flag.Uint("port", uint(agent.Port), "vsock port on the host")
	retry := flag.Duration("retry", 500*time.Millisecond, "delay between connection attempts")
	attempts := flag.Int("attempts", 60, "connection attempts before giving up")
	flag.Parse()

	if err := run(uint32(*port), *retry, *attempts); err != nil {
		fmt.Fprintf(os.Stderr, "hangar-agent: %v\n", err)
		os.Exit(1)
	}
}

func run(port uint32, retry time.Duration, attempts int) error {
	// The agent starts before sysinit.target so that a stuck boot is still
	// observable, which means the vsock device may not be probed yet and the
	// host may not be listening. A first failure is expected, not fatal.
	var conn *os.File
	var err error
	for i := 0; i < attempts; i++ {
		conn, err = vsock.Dial(vsock.CIDHost, port)
		if err == nil {
			break
		}
		time.Sleep(retry)
	}
	if conn == nil {
		return fmt.Errorf("no host on vsock port %d after %d attempts: %w", port, attempts, err)
	}
	defer conn.Close()

	if err := sendHello(conn); err != nil {
		return err
	}
	return serve(conn)
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
func serve(conn *os.File) error {
	br := bufio.NewReader(conn)
	for {
		line, err := br.ReadBytes('\n')
		if err != nil {
			// A closed channel is the host going away, which is how an agent
			// normally ends its life.
			return nil
		}
		var req agent.Request
		if err := json.Unmarshal(line, &req); err != nil {
			continue
		}
		resp := handle(req)
		b, err := json.Marshal(resp)
		if err != nil {
			continue
		}
		if _, err := conn.Write(append(b, '\n')); err != nil {
			return nil
		}
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

	err := cmd.Run()
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
// own clock. Measuring here rather than on the host excludes QEMU startup and
// host scheduling, so it answers "how long did the guest take to become
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
