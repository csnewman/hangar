package frontendapi

import (
	"context"
	"time"

	"github.com/coder/websocket"
)

// keepAlive pings a browser's socket every h.socketPing, and closes it when
// a pong does not come back as soon again. A phone put away, or a network
// gone, leaves a socket that nothing at the other end will close, and what
// it holds in the environment -- a terminal's window, the Code service's
// connection, a desktop's viewer -- would be held for good. It returns when
// ctx ends; the socket must be read meanwhile, which is where pongs arrive.
func (h *handler) keepAlive(ctx context.Context, c *websocket.Conn) {
	every := h.socketPing
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

// answerPing answers a browser's own check that its socket is still there,
// a text message "ping", with "pong", and says whether msg was one. A
// browser cannot see the socket's pings, so a network gone would leave it
// showing a terminal or a desktop as live, and sending what is typed into
// nothing, until its own TCP gave up minutes later.
func answerPing(ctx context.Context, c *websocket.Conn, kind websocket.MessageType, msg []byte) bool {
	if kind != websocket.MessageText || string(msg) != "ping" {
		return false
	}
	c.Write(ctx, websocket.MessageText, []byte("pong"))
	return true
}
