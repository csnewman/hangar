//go:build linux

package procs_test

import (
	"bufio"
	"encoding/json"
	"net"
	"os"
	"os/exec"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/procs"
)

func ask(t *testing.T, c net.Conn, r *bufio.Reader, req procs.Request) procs.Reply {
	t.Helper()
	b, _ := json.Marshal(req)
	if _, err := c.Write(append(b, '\n')); err != nil {
		t.Fatal(err)
	}
	line, err := r.ReadBytes('\n')
	if err != nil {
		t.Fatal(err)
	}
	var reply procs.Reply
	if err := json.Unmarshal(line, &reply); err != nil {
		t.Fatal(err)
	}
	return reply
}

func find(ps []procs.Process, pid int) *procs.Process {
	for i := range ps {
		if ps[i].PID == pid {
			return &ps[i]
		}
	}
	return nil
}

func TestListAndStop(t *testing.T) {
	// One process busy on a CPU, and one asleep.
	busy := exec.Command("sh", "-c", "while :; do :; done")
	idle := exec.Command("sleep", "60")
	for _, c := range []*exec.Cmd{busy, idle} {
		if err := c.Start(); err != nil {
			t.Fatal(err)
		}
		defer c.Process.Kill()
	}

	srv := procs.NewServer()
	a, b := net.Pipe()
	go srv.Serve(b)
	defer a.Close()
	r := bufio.NewReader(a)

	first := ask(t, a, r, procs.Request{Op: "list"})
	if first.Error != "" || first.CPUs < 1 || first.MemoryBytes <= 0 {
		t.Fatalf("first listing: %+v", first.Error)
	}
	if me := find(first.Processes, os.Getpid()); me == nil || !me.Protected {
		t.Errorf("the serving process is not marked protected: %+v", me)
	}
	p := find(first.Processes, idle.Process.Pid)
	if p == nil || p.Command != "sleep 60" || p.Name != "sleep" || p.PPID != os.Getpid() {
		t.Fatalf("the sleeping process: %+v", p)
	}
	time.Sleep(time.Second)
	second := ask(t, a, r, procs.Request{Op: "list"})
	if b := find(second.Processes, busy.Process.Pid); b == nil || b.CPUPercent < 10 {
		t.Errorf("the busy process: %+v", b)
	}
	if s := find(second.Processes, idle.Process.Pid); s == nil || s.CPUPercent > 5 {
		t.Errorf("the sleeping process: %+v", s)
	}

	if reply := ask(t, a, r, procs.Request{Op: "kill", PID: idle.Process.Pid}); reply.Error != "" {
		t.Fatal(reply.Error)
	}
	done := make(chan error, 1)
	go func() { done <- idle.Wait() }()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("the process was not stopped")
	}
	if reply := ask(t, a, r, procs.Request{Op: "kill", PID: 1}); reply.Error == "" {
		t.Error("init was stopped")
	}
	if reply := ask(t, a, r, procs.Request{Op: "kill", PID: busy.Process.Pid, Signal: "HUP"}); reply.Error == "" {
		t.Error("an unknown signal was sent")
	}
}
