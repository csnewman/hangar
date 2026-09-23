package main

import (
	"fmt"
	"log/slog"
	"os"
	"time"

	"github.com/csnewman/hangar/internal/desktop"
	"github.com/csnewman/hangar/internal/vsock"
)

// serveDesktop answers the host's desktop connections for as long as the guest
// runs. Its VNC server starts on the first of them.
func serveDesktop() {
	srv := desktop.NewServer("dev", slog.New(slog.NewTextHandler(os.Stderr, nil)))
	for {
		ln, err := vsock.Listen(vsock.CIDAny, desktop.Port)
		if err != nil {
			fmt.Fprintf(os.Stderr, "hangar-agent: desktop: %v\n", err)
			time.Sleep(5 * time.Second)
			continue
		}
		for {
			conn, _, err := ln.Accept(0)
			if err != nil {
				fmt.Fprintf(os.Stderr, "hangar-agent: desktop: %v\n", err)
				break
			}
			go srv.Serve(conn)
		}
		ln.Close()
	}
}
