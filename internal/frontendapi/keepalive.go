package frontendapi

import (
	"context"

	"github.com/coder/websocket"

	"github.com/csnewman/hangar/internal/termws"
)

// keepAlive pings a browser's socket every h.socketPing (termws.KeepAlive).
func (h *handler) keepAlive(ctx context.Context, c *websocket.Conn) {
	termws.KeepAlive(ctx, c, h.socketPing)
}

// answerPing answers a browser's "ping" (termws.AnswerPing).
func answerPing(ctx context.Context, c *websocket.Conn, kind websocket.MessageType, msg []byte) bool {
	return termws.AnswerPing(ctx, c, kind, msg)
}
