// Package termws carries a terminal session to a WebSocket and back, for
// the web UI's API and the client API alike.
//
// The first message to the client is the attach's reply (terminal.Reply)
// as JSON text. After it every message is binary, one frame each: the frame
// type byte followed by its payload, in both directions. The length the
// session's stream gives each frame is the message's own, so the client
// never sees it. A client may send the text "ping" at any time and is
// answered "pong": a browser cannot see the socket's own pings.
package termws

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"time"

	"github.com/coder/websocket"

	"github.com/csnewman/hangar/internal/terminal"
)

// Ping is how often a socket is pinged when the caller gives no interval.
const Ping = 20 * time.Second

// Bridge carries the session on stream (whose reader is br, past the
// attach's reply) to c and back until either ends. It closes neither.
func Bridge(ctx context.Context, c *websocket.Conn, stream io.Writer, br *bufio.Reader,
	reply terminal.Reply, ping time.Duration) {
	c.SetReadLimit(terminal.MaxFrame + 1)
	first, _ := json.Marshal(reply)
	if err := c.Write(ctx, websocket.MessageText, first); err != nil || reply.Err != "" {
		c.Close(websocket.StatusNormalClosure, "")
		return
	}

	ctx, cancel := context.WithCancel(context.WithoutCancel(ctx))
	defer cancel()
	go KeepAlive(ctx, c, ping)
	go func() {
		defer cancel()
		for {
			typ, payload, err := terminal.ReadFrame(br)
			if err != nil {
				return
			}
			msg := append([]byte{typ}, payload...)
			if err := c.Write(ctx, websocket.MessageBinary, msg); err != nil {
				return
			}
			if typ == terminal.FrameExit {
				c.Close(websocket.StatusNormalClosure, "the shell exited")
				return
			}
		}
	}()
	for {
		kind, msg, err := c.Read(ctx)
		if err != nil {
			return
		}
		if AnswerPing(ctx, c, kind, msg) || kind != websocket.MessageBinary || len(msg) == 0 {
			continue
		}
		switch msg[0] {
		case terminal.FrameInput, terminal.FrameResize, terminal.FramePrefer:
			if terminal.WriteFrame(stream, msg[0], msg[1:]) != nil {
				return
			}
		}
	}
}

// KeepAlive pings a socket every interval (Ping when it is zero), and
// closes it when a pong does not come back as soon again. A laptop shut, or
// a network gone, leaves a socket that nothing at the other end will close,
// and what it holds in the environment would be held for good. It returns
// when ctx ends; the socket must be read meanwhile, which is where pongs
// arrive.
func KeepAlive(ctx context.Context, c *websocket.Conn, every time.Duration) {
	if every == 0 {
		every = Ping
	}
	for {
		select {
		case <-ctx.Done():
			return
		case <-time.After(every):
		}
		pctx, cancel := context.WithTimeout(ctx, every)
		err := c.Ping(pctx)
		cancel()
		if err != nil {
			c.CloseNow()
			return
		}
	}
}

// AnswerPing answers a client's own check that its socket is still there, a
// text message "ping", with "pong", and says whether msg was one.
func AnswerPing(ctx context.Context, c *websocket.Conn, kind websocket.MessageType, msg []byte) bool {
	if kind != websocket.MessageText || string(msg) != "ping" {
		return false
	}
	c.Write(ctx, websocket.MessageText, []byte("pong"))
	return true
}
