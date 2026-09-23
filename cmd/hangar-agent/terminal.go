package main

import (
	"fmt"
	"log/slog"
	"net"
	"os"
	"os/user"
	"path/filepath"
	"strconv"
	"time"

	"github.com/csnewman/hangar/internal/terminal"
	"github.com/csnewman/hangar/internal/vsock"
)

// terminalSocket is where programs in the guest reach the same sessions the
// host does: the editor's terminal panel, for one.
const terminalSocket = "/run/hangar/terminal.sock"

// serveTerminals answers terminal connections for as long as the guest runs,
// from the host over vsock and from inside the guest over terminalSocket.
//
// Unlike the control channel the host dials in, since a terminal is opened
// when someone asks for one. Its sessions live here, in the guest, so they
// outlive every connection to them: the browser tab, the worker and the
// control plane can all come and go while a shell carries on.
func serveTerminals() {
	mgr := terminal.NewManager(terminal.LoginShell("dev"), slog.New(slog.NewTextHandler(os.Stderr, nil)))
	go serveTerminalSocket(mgr, "dev")
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

// serveTerminalSocket serves the sessions on terminalSocket, which only owner
// may use: whoever connects gets that user's shells.
func serveTerminalSocket(mgr *terminal.Manager, owner string) {
	for {
		if err := listenTerminalSocket(mgr, owner); err != nil {
			fmt.Fprintf(os.Stderr, "hangar-agent: terminal socket: %v\n", err)
		}
		time.Sleep(5 * time.Second)
	}
}

func listenTerminalSocket(mgr *terminal.Manager, owner string) error {
	u, err := user.Lookup(owner)
	if err != nil {
		return err
	}
	uid, _ := strconv.Atoi(u.Uid)
	gid, _ := strconv.Atoi(u.Gid)
	if err := os.MkdirAll(filepath.Dir(terminalSocket), 0o755); err != nil {
		return err
	}
	_ = os.Remove(terminalSocket)
	ln, err := net.Listen("unix", terminalSocket)
	if err != nil {
		return err
	}
	defer ln.Close()
	if err := os.Chown(terminalSocket, uid, gid); err != nil {
		return err
	}
	if err := os.Chmod(terminalSocket, 0o600); err != nil {
		return err
	}
	for {
		conn, err := ln.Accept()
		if err != nil {
			return err
		}
		go mgr.Serve(conn)
	}
}
