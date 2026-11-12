package main

import (
	"fmt"
	"log/slog"
	"os"
	"time"

	"github.com/csnewman/hangar/internal/oomscore"
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
	srv.Prepare = ranked(oomscore.User)
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
			conn, cid, err := ln.Accept(0)
			if err != nil {
				fmt.Fprintf(os.Stderr, "hangar-agent: ssh: %v\n", err)
				break
			}
			// The server asks nothing of whoever connects, trusting the
			// gateway to have signed them in: a process in the guest can
			// dial it too, through vsock's loopback, and is refused.
			if cid != vsock.CIDHost {
				fmt.Fprintf(os.Stderr, "hangar-agent: ssh: refused a connection from CID %d, not the host\n", cid)
				conn.Close()
				continue
			}
			go srv.Serve(sshd.FileConn(conn))
		}
		ln.Close()
	}
}
