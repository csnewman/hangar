package guestport_test

import (
	"errors"
	"io"
	"net"
	"testing"

	"github.com/csnewman/hangar/internal/guestport"
)

// serve answers one request on the far end of a pipe, as the agent would.
func serve(t *testing.T) net.Conn {
	t.Helper()
	host, guest := net.Pipe()
	go guestport.Serve(guest)
	t.Cleanup(func() { host.Close() })
	return host
}

func TestConnects(t *testing.T) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer ln.Close()
	go func() {
		c, err := ln.Accept()
		if err != nil {
			return
		}
		defer c.Close()
		c.Write([]byte("hello from inside"))
	}()

	conn := serve(t)
	if err := guestport.Request(conn, uint16(ln.Addr().(*net.TCPAddr).Port)); err != nil {
		t.Fatal(err)
	}
	got, err := io.ReadAll(conn)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != "hello from inside" {
		t.Errorf("read %q", got)
	}
}

func TestRefused(t *testing.T) {
	// A port just released, which nothing listens on.
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	port := uint16(ln.Addr().(*net.TCPAddr).Port)
	ln.Close()

	err = guestport.Request(serve(t), port)
	if !errors.Is(err, guestport.ErrRefused) {
		t.Errorf("a closed port: %v, want ErrRefused", err)
	}
}
