//go:build krun

// Command hangar-krun runs one machine under libkrun, from the JSON
// description the worker writes (krun.Config). libkrun takes the process
// over, and it exits when the guest does.
//
// libkrun's API changed whole at 2.0: libkrun1_krun.go speaks 1.x's, which
// the Mac builds against, and libkrun2_krun.go (the krun2 tag) 2.x's.
package main

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"

	"github.com/csnewman/hangar/internal/krun"
)

// suspend writes the running machine to a snapshot in a directory, leaving
// it paused; start sets it once there is a machine, and only where libkrun
// takes snapshots.
var suspend = func(string) error { return errors.New("this libkrun takes no snapshots") }

func main() {
	gpu := hasGPU()
	var vulkan string
	if gpu {
		vulkan = chooseVulkan()
	}
	if len(os.Args) == 2 && os.Args[1] == "-features" {
		json.NewEncoder(os.Stdout).Encode(krun.Features{GPU: gpu, Vulkan: vulkan, Zink: gpu && canZink()})
		return
	}
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: hangar-krun CONFIG.json | -features")
		os.Exit(2)
	}
	// The worker holds the other end of standard input: when the worker
	// goes, however it goes, so does the machine, rather than running on
	// with nothing to supervise it. macOS has no parent-death signal.
	if st, err := os.Stdin.Stat(); err == nil && st.Mode()&os.ModeNamedPipe != 0 {
		go control(bufio.NewScanner(os.Stdin), os.NewFile(3, "reply"))
	}
	b, err := os.ReadFile(os.Args[1])
	if err == nil {
		var c krun.Config
		if err = json.Unmarshal(b, &c); err == nil {
			err = start(c)
		}
	}
	if err != nil {
		fmt.Fprintf(os.Stderr, "hangar-krun: %v\n", err)
		os.Exit(1)
	}
}

// control serves the worker's requests, a line each on standard input, and
// answers each on reply:
//
//	suspend DIR   snapshot the machine into DIR; "ok", or "error: ..." with
//	              the machine running on
//	exit          stop, once the worker has recorded the snapshot
func control(requests *bufio.Scanner, reply *os.File) {
	for requests.Scan() {
		verb, arg, _ := strings.Cut(requests.Text(), " ")
		switch verb {
		case "suspend":
			if err := suspend(arg); err != nil {
				fmt.Fprintf(reply, "error: %v\n", err)
				continue
			}
			fmt.Fprintln(reply, "ok")
		case "exit":
			os.Exit(0)
		default:
			fmt.Fprintf(reply, "error: unknown request %q\n", verb)
		}
	}
	fmt.Fprintln(os.Stderr, "hangar-krun: the worker has gone; stopping the machine")
	os.Exit(1)
}
