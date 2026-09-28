package main

import (
	"fmt"
	"os"
	"time"

	"github.com/csnewman/hangar/internal/guestport"
	"github.com/csnewman/hangar/internal/sshd"
	"github.com/csnewman/hangar/internal/vsock"
)

// servePorts connects the host to the environment's own TCP ports, for its
// web servers reached through Hangar, for as long as the guest runs.
func servePorts() {
	for {
		ln, err := vsock.Listen(vsock.CIDAny, guestport.Port)
		if err != nil {
			fmt.Fprintf(os.Stderr, "hangar-agent: ports: %v\n", err)
			time.Sleep(5 * time.Second)
			continue
		}
		for {
			conn, _, err := ln.Accept(0)
			if err != nil {
				fmt.Fprintf(os.Stderr, "hangar-agent: ports: %v\n", err)
				break
			}
			go guestport.Serve(sshd.FileConn(conn))
		}
		ln.Close()
	}
}
