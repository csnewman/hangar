// Package terminal runs interactive shells that outlive the connections
// watching them, and speaks the protocol that carries them.
//
// A session is a shell on a pseudo-terminal. It is started by the first
// attach that asks for one and runs until its shell exits; connections come
// and go. Any number may be attached at once -- two browser tabs, or a tab
// refreshed while the old one is still closing -- and each sees the same
// output and may type. The size is whichever attached connection last sent
// one, as tmux does by default.
//
// A connection that attaches is first sent the session's recent output, so a
// refreshed page shows what was there. That is a replay of bytes, not a
// picture of the screen: a full-screen program is additionally told the
// window changed, which makes it draw itself again.
//
// Live output is passed through untouched, so everything a terminal on the
// other end understands -- images, the kitty keyboard protocol, clipboard
// and hyperlink sequences -- reaches it exactly as the program wrote it.
//
// # Protocol
//
// A connection starts with one JSON line, a Request, answered by one JSON
// line, a Reply. After a successful attach both directions carry frames: a
// type byte, a four-byte big-endian length, and that many bytes.
package terminal

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"time"
)

// Port is the vsock port the guest agent serves terminals on. The host dials
// it: a terminal is opened when someone asks for one, not when the guest
// boots.
const Port uint32 = 8101

// Request operations.
const (
	OpAttach = "attach"
	OpList   = "list"
	OpClose  = "close"
)

// Request opens a connection.
type Request struct {
	Op string `json:"op"`
	// Session names the session to attach to or close. Attaching with none
	// starts a new session.
	Session string `json:"session,omitempty"`
	Cols    uint16 `json:"cols,omitempty"`
	Rows    uint16 `json:"rows,omitempty"`
	// User and Dir apply to a new session: whose shell, and where it starts.
	// An empty User is the environment's usual user.
	User string `json:"user,omitempty"`
	Dir  string `json:"dir,omitempty"`
}

// Session describes one session.
type Session struct {
	ID string `json:"id"`
	// Title is what the shell or the program in it last set the window
	// title to.
	Title   string    `json:"title"`
	Created time.Time `json:"created"`
	// Clients is how many connections are attached.
	Clients int    `json:"clients"`
	Cols    uint16 `json:"cols"`
	Rows    uint16 `json:"rows"`
}

// Reply answers a Request. Err is set if it failed; otherwise Session is the
// session attached to, or Sessions every session, for a list.
type Reply struct {
	Err      string    `json:"err,omitempty"`
	Session  *Session  `json:"session,omitempty"`
	Sessions []Session `json:"sessions,omitempty"`
}

// Frame types.
const (
	// FrameInput carries keystrokes and pastes to the shell.
	FrameInput byte = 'i'
	// FrameResize carries a new size: columns then rows, two bytes each.
	FrameResize byte = 'r'
	// FrameOutput carries what the shell wrote.
	FrameOutput byte = 'o'
	// FrameExit says the shell exited, with its status as four bytes. It is
	// the last frame on the connection.
	FrameExit byte = 'x'
)

// MaxFrame is the largest payload a frame may carry.
const MaxFrame = 1 << 20

var errTooLarge = errors.New("terminal frame too large")

// WriteFrame writes one frame.
func WriteFrame(w io.Writer, typ byte, payload []byte) error {
	if len(payload) > MaxFrame {
		return errTooLarge
	}
	var hdr [5]byte
	hdr[0] = typ
	binary.BigEndian.PutUint32(hdr[1:], uint32(len(payload)))
	if _, err := w.Write(hdr[:]); err != nil {
		return err
	}
	_, err := w.Write(payload)
	return err
}

// ReadFrame reads one frame.
func ReadFrame(r io.Reader) (byte, []byte, error) {
	var hdr [5]byte
	if _, err := io.ReadFull(r, hdr[:]); err != nil {
		return 0, nil, err
	}
	n := binary.BigEndian.Uint32(hdr[1:])
	if n > MaxFrame {
		return 0, nil, errTooLarge
	}
	payload := make([]byte, n)
	if _, err := io.ReadFull(r, payload); err != nil {
		return 0, nil, err
	}
	return hdr[0], payload, nil
}

// ResizePayload encodes a size for FrameResize.
func ResizePayload(cols, rows uint16) []byte {
	b := make([]byte, 4)
	binary.BigEndian.PutUint16(b, cols)
	binary.BigEndian.PutUint16(b[2:], rows)
	return b
}

// ParseResize decodes a FrameResize payload.
func ParseResize(b []byte) (cols, rows uint16, err error) {
	if len(b) != 4 {
		return 0, 0, fmt.Errorf("resize frame of %d bytes", len(b))
	}
	return binary.BigEndian.Uint16(b), binary.BigEndian.Uint16(b[2:]), nil
}
