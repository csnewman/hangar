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
