package oomscore_test

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"testing"

	"github.com/csnewman/hangar/internal/oomscore"
)

// The test binary stands in for the agent: run with Arg, it Execs.
func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == oomscore.Arg {
		err := oomscore.Exec(os.Args[2:])
		fmt.Fprintln(os.Stderr, err)
		os.Exit(127)
	}
	os.Exit(m.Run())
}

func exe(t *testing.T) string {
	t.Helper()
	if _, err := os.Stat("/proc/self/oom_score_adj"); err != nil {
		t.Skip("no OOM scores here")
	}
	p, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	return p
}

func TestAProcessAndWhatItStartsHaveItsScore(t *testing.T) {
	cmd := exec.Command("sh", "-c", `cat /proc/self/oom_score_adj; sh -c 'cat /proc/self/oom_score_adj'`)
	oomscore.Wrap(exe(t), cmd, 300)
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("%v: %s", err, out)
	}
	if got := string(out); got != "300\n300\n" {
		t.Fatalf("scores = %q, want 300 for the process and its child", got)
	}
}

func TestTheProgramGetsItsOwnArgumentsAndEnvironment(t *testing.T) {
	cmd := exec.Command("sh", "-c", `printf '%s|%s|%s|%s' "$0" "$1" "$X" "$(pwd)"`, "zero", "one")
	cmd.Env = []string{"X=ex", "PATH=" + os.Getenv("PATH")}
	cmd.Dir = t.TempDir()
	oomscore.Wrap(exe(t), cmd, 0)
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("%v: %s", err, out)
	}
	dir, err := filepath.EvalSymlinks(cmd.Dir)
	if err != nil {
		t.Fatal(err)
	}
	if want := "zero|one|ex|" + dir; string(out) != want {
		t.Fatalf("output = %q, want %q", out, want)
	}
}

func TestAMissingProgramIsReportedByStart(t *testing.T) {
	cmd := exec.Command("/no/such/program")
	oomscore.Wrap(exe(t), cmd, 0)
	if err := cmd.Start(); err == nil {
		cmd.Wait()
		t.Fatal("started a program that is not there")
	}
}
