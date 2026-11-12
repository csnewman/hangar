package guestport_test

import (
	"encoding/binary"
	"errors"
	"io"
	"net"
	"testing"

	"github.com/csnewman/hangar/internal/guestport"
)

// guest answers one request on the far end of a pipe with answer, as the
// agent would, then writes then.
func guest(t *testing.T, want uint16, answer byte, then string) net.Conn {
	t.Helper()
	host, g := net.Pipe()
	go func() {
		defer g.Close()
		var req [2]byte
		if _, err := io.ReadFull(g, req[:]); err != nil || binary.BigEndian.Uint16(req[:]) != want {
			return
		}
		g.Write(append([]byte{answer}, then...))
	}()
	t.Cleanup(func() { host.Close() })
	return host
}

func TestConnects(t *testing.T) {
	conn := guest(t, 8080, guestport.Connected, "hello from inside")
	if err := guestport.Request(conn, 8080); err != nil {
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
	err := guestport.Request(guest(t, 3000, guestport.Refused, ""), 3000)
	if !errors.Is(err, guestport.ErrRefused) {
		t.Errorf("a closed port: %v, want ErrRefused", err)
	}
	err = guestport.Request(guest(t, 3000, guestport.Failed, ""), 3000)
	if err == nil || errors.Is(err, guestport.ErrRefused) {
		t.Errorf("a port that failed: %v", err)
	}
}
