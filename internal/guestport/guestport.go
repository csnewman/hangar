// Package guestport reaches a TCP port inside an environment: its agent
// listens on vsock Port and, for each connection, connects to the port the
// host names on the guest's own loopback, then carries bytes both ways.
//
// The host writes the port, two bytes big-endian. The agent answers one
// byte: Connected, then the stream is the connection; or Refused, when
// nothing listens on the port, or Failed, and it closes.
package guestport

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"net"
	"strconv"
	"sync"
	"syscall"
	"time"
)

// Port is the vsock port the agent serves this on.
const Port uint32 = 8108

// Answers to a request.
const (
	Connected byte = 0
	Refused   byte = 1
	Failed    byte = 2
)

// ErrRefused is that nothing in the guest listens on the port.
var ErrRefused = errors.New("nothing in the environment is listening on the port")

// dialTimeout bounds the agent's connection to the port.
const dialTimeout = 10 * time.Second

// Request asks for a connection to port over conn, which then carries it.
func Request(conn net.Conn, port uint16) error {
	var req [2]byte
	binary.BigEndian.PutUint16(req[:], port)
	if _, err := conn.Write(req[:]); err != nil {
		return err
	}
	var answer [1]byte
	if _, err := io.ReadFull(conn, answer[:]); err != nil {
		return fmt.Errorf("asking the environment for port %d: %w", port, err)
	}
	switch answer[0] {
	case Connected:
		return nil
	case Refused:
		return fmt.Errorf("port %d: %w", port, ErrRefused)
	}
	return fmt.Errorf("the environment could not connect to its port %d", port)
}

// Serve answers one request on conn, in the guest, and carries the
// connection until either side closes it.
func Serve(conn net.Conn) {
	defer conn.Close()
	var req [2]byte
	if _, err := io.ReadFull(conn, req[:]); err != nil {
		return
	}
	port := binary.BigEndian.Uint16(req[:])
	target, err := net.DialTimeout("tcp", net.JoinHostPort("localhost", strconv.Itoa(int(port))), dialTimeout)
	if err != nil {
		answer := Failed
		if errors.Is(err, syscall.ECONNREFUSED) {
			answer = Refused
		}
		conn.Write([]byte{answer})
		return
	}
	defer target.Close()
	if _, err := conn.Write([]byte{Connected}); err != nil {
		return
	}
	var wg sync.WaitGroup
	wg.Add(2)
	go func() {
		defer wg.Done()
		io.Copy(target, conn)
		closeWrite(target)
	}()
	go func() {
		defer wg.Done()
		io.Copy(conn, target)
		closeWrite(conn)
	}()
	wg.Wait()
}

// closeWrite passes an end of stream on, where the connection can say so
// while still reading.
func closeWrite(c net.Conn) {
	if cw, ok := c.(interface{ CloseWrite() error }); ok {
		cw.CloseWrite()
		return
	}
	c.Close()
}
