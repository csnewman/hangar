package main

import (
	"fmt"
	"log/slog"
	"os"
	"time"

	"github.com/csnewman/hangar/internal/code"
	"github.com/csnewman/hangar/internal/vsock"
)

// serveCode answers the host's Code tab connections for as long as the guest
// runs. Its service starts on the first of them.
func serveCode() {
	srv := code.NewServer("dev", slog.New(slog.NewTextHandler(os.Stderr, nil)))
	for {
		ln, err := vsock.Listen(vsock.CIDAny, code.Port)
		if err != nil {
			fmt.Fprintf(os.Stderr, "hangar-agent: code: %v\n", err)
			time.Sleep(5 * time.Second)
			continue
		}
		for {
			conn, _, err := ln.Accept(0)
			if err != nil {
				fmt.Fprintf(os.Stderr, "hangar-agent: code: %v\n", err)
				break
			}
			go srv.Serve(conn)
		}
		ln.Close()
	}
}
