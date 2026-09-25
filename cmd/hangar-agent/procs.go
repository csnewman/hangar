package main

import (
	"fmt"
	"os"
	"time"

	"github.com/csnewman/hangar/internal/procs"
	"github.com/csnewman/hangar/internal/vsock"
)

// serveProcs answers the host's questions about what runs in the guest,
// for as long as the guest runs.
func serveProcs() {
	srv := procs.NewServer()
	for {
		ln, err := vsock.Listen(vsock.CIDAny, procs.Port)
		if err != nil {
			fmt.Fprintf(os.Stderr, "hangar-agent: procs: %v\n", err)
			time.Sleep(5 * time.Second)
			continue
		}
		for {
			conn, _, err := ln.Accept(0)
			if err != nil {
				fmt.Fprintf(os.Stderr, "hangar-agent: procs: %v\n", err)
				break
			}
			go srv.Serve(conn)
		}
		ln.Close()
	}
}
