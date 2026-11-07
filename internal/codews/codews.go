// Package codews carries a connection to an environment's Code service to
// a WebSocket and back, for the web UI's API and the client API alike.
//
// Each binary message is one of the service's frames, whose length the
// stream carries as a four-byte prefix and the WebSocket carries as the
// message's own; nothing else of them is read here. A client may send the
// text "ping" at any time and is answered "pong".
package codews

import (
	"context"
	"encoding/binary"
	"io"
	"sync/atomic"
	"time"

	"github.com/coder/websocket"

	"github.com/csnewman/hangar/internal/termws"
)

// FrameMax is the largest frame either side sends: a whole file's shared
// document on first sync.
const FrameMax = 64 << 20

// Bridge carries frames between c and stream until either ends, then
// closes c, saying so when the service never answered. It does not close
// stream.
func Bridge(ctx context.Context, c *websocket.Conn, stream io.ReadWriter, ping time.Duration) {
	c.SetReadLimit(FrameMax)
	ctx, cancel := context.WithCancel(context.WithoutCancel(ctx))
	defer cancel()
	go termws.KeepAlive(ctx, c, ping)
	var received atomic.Bool
	done := make(chan struct{}, 2)
	go func() {
		defer func() { done <- struct{}{} }()
		var hdr [4]byte
		for {
			if _, err := io.ReadFull(stream, hdr[:]); err != nil {
				return
			}
			n := binary.BigEndian.Uint32(hdr[:])
			if n > FrameMax {
				return
			}
			msg := make([]byte, n)
			if _, err := io.ReadFull(stream, msg); err != nil {
				return
			}
			received.Store(true)
			if err := c.Write(ctx, websocket.MessageBinary, msg); err != nil {
				return
			}
		}
	}()
	go func() {
		defer func() { done <- struct{}{} }()
		for {
			kind, msg, err := c.Read(ctx)
			if err != nil {
				return
			}
			if termws.AnswerPing(ctx, c, kind, msg) || kind != websocket.MessageBinary {
				continue
			}
			var hdr [4]byte
			binary.BigEndian.PutUint32(hdr[:], uint32(len(msg)))
			if _, err := stream.Write(append(hdr[:], msg...)); err != nil {
				return
			}
		}
	}()
	<-done
	if !received.Load() {
		c.Close(websocket.StatusTryAgainLater, "the environment's Code service is not available")
		return
	}
	c.Close(websocket.StatusNormalClosure, "")
}
