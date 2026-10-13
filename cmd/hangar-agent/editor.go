package main

import (
	"fmt"
	"log/slog"
	"os"
	"time"

	"github.com/csnewman/hangar/internal/oomscore"
	"github.com/csnewman/hangar/internal/vscode"
	"github.com/csnewman/hangar/internal/vsock"
)

// serveEditor answers the host's editor connections for as long as the guest
// runs. The editor itself starts on the first of them.
func serveEditor() {
	// VS Code's server runs the user's terminals and extensions itself, and
	// they would inherit any protection it had, so it is ranked with them.
	srv := vscode.NewServer("dev", slog.New(slog.NewTextHandler(os.Stderr, nil)))
	srv.Prepare = ranked(oomscore.User)
	for {
		ln, err := vsock.Listen(vsock.CIDAny, vscode.Port)
		if err != nil {
			fmt.Fprintf(os.Stderr, "hangar-agent: editor: %v\n", err)
			time.Sleep(5 * time.Second)
			continue
		}
		for {
			conn, _, err := ln.Accept(0)
			if err != nil {
				fmt.Fprintf(os.Stderr, "hangar-agent: editor: %v\n", err)
				break
			}
			go srv.Serve(conn)
		}
		ln.Close()
	}
}
