package main

import (
	"fmt"
	"log/slog"
	"os"
	"time"

	"github.com/csnewman/hangar/internal/sshd"
	"github.com/csnewman/hangar/internal/vsock"
)

// serveSSH answers SSH from the host's gateway, as the environment's user,
// for as long as the guest runs.
func serveSSH() {
	srv, err := sshd.NewServer("dev", slog.New(slog.NewTextHandler(os.Stderr, nil)))
	if err != nil {
		fmt.Fprintf(os.Stderr, "hangar-agent: ssh: %v\n", err)
		return
	}
	if exe, err := os.Executable(); err == nil {
		srv.SFTP = exe + " sftp-server"
	}
	for {
		ln, err := vsock.Listen(vsock.CIDAny, sshd.Port)
		if err != nil {
			fmt.Fprintf(os.Stderr, "hangar-agent: ssh: %v\n", err)
			time.Sleep(5 * time.Second)
			continue
		}
		for {
			conn, _, err := ln.Accept(0)
			if err != nil {
				fmt.Fprintf(os.Stderr, "hangar-agent: ssh: %v\n", err)
				break
			}
			go srv.Serve(sshd.FileConn(conn))
		}
		ln.Close()
	}
}
