package main

import (
	"fmt"
	"log/slog"
	"os"
	"time"

	"github.com/csnewman/hangar/internal/terminal"
	"github.com/csnewman/hangar/internal/vsock"
)

// serveTerminals answers the host's terminal connections for as long as the
// guest runs.
//
// Unlike the control channel the host dials in, since a terminal is opened
// when someone asks for one. Its sessions live here, in the guest, so they
// outlive every connection to them: the browser tab, the worker and the
// control plane can all come and go while a shell carries on.
func serveTerminals() {
	mgr := terminal.NewManager(terminal.LoginShell("dev"), slog.New(slog.NewTextHandler(os.Stderr, nil)))
	for {
		ln, err := vsock.Listen(vsock.CIDAny, terminal.Port)
		if err != nil {
			fmt.Fprintf(os.Stderr, "hangar-agent: terminals: %v\n", err)
			time.Sleep(5 * time.Second)
			continue
		}
		for {
			conn, _, err := ln.Accept(0)
			if err != nil {
				fmt.Fprintf(os.Stderr, "hangar-agent: terminals: %v\n", err)
				break
			}
			go mgr.Serve(conn)
		}
		ln.Close()
	}
}
