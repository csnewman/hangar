// Package hangarsync speaks /dev/hangar-sync, through which the guest kernel
// (fs/hangar/sync.c in internal/kernel/tree) has the agent take locks on
// shared files for the environment as a whole.
//
// The layout is struct hangar_sync_msg in include/uapi/linux/hangar_sync.h:
// a fixed header, then the path. Each read of the device returns one message
// and each write gives it one.
package hangarsync

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io"
)

// Device is where the kernel serves the protocol.
const Device = "/dev/hangar-sync"

// Message types.
const (
	// Kernel to agent.
	OpLock   = 1 // take the file's lock for the environment; reply
	OpIdle   = 2 // no process here holds a lock on the file
	OpCancel = 3 // the Lock with this ID is no longer wanted

	// Agent to kernel.
	OpReply      = 16 // the answer to a Lock
	OpAddPath    = 17 // files at or under the path are shared
	OpClearPaths = 18 // no path is shared
)

// Flags on a Lock.
const (
	FlagWait  = 1 // the caller waits for the lock
	FlagFlock = 2 // flock(2) rather than a POSIX or OFD lock
)

// HeaderSize is the fixed part of a message.
const HeaderSize = 56

// MaxSize is the largest message: a header and a PATH_MAX path.
const MaxSize = HeaderSize + 4096

// Msg is one message.
type Msg struct {
	Op     uint32
	ID     uint64
	Result int32 // a Reply's: 0, or a negative errno
	Type   uint32
	Flags  uint32
	PID    uint32
	Start  uint64
	End    uint64
	Path   string
}

// Marshal lays a message out as the device takes it.
func (m Msg) Marshal() []byte {
	b := make([]byte, HeaderSize+len(m.Path))
	le := binary.LittleEndian
	le.PutUint32(b[0:], uint32(len(b)))
	le.PutUint32(b[4:], m.Op)
	le.PutUint64(b[8:], m.ID)
	le.PutUint32(b[16:], uint32(m.Result))
	le.PutUint32(b[20:], m.Type)
	le.PutUint32(b[24:], m.Flags)
	le.PutUint32(b[28:], m.PID)
	le.PutUint64(b[32:], m.Start)
	le.PutUint64(b[40:], m.End)
	le.PutUint32(b[48:], uint32(len(m.Path)))
	copy(b[HeaderSize:], m.Path)
	return b
}

// Reader reads messages. The device returns one whole message for each
// read, given room for the largest; a stream (as in tests) may split or join
// them, which Reader also takes.
type Reader struct {
	r   io.Reader
	buf []byte
	n   int
}

// NewReader reads messages from r.
func NewReader(r io.Reader) *Reader {
	return &Reader{r: r, buf: make([]byte, 2*MaxSize)}
}

// Next returns the next message.
func (rd *Reader) Next() (Msg, error) {
	le := binary.LittleEndian
	for {
		if rd.n >= 4 {
			size := int(le.Uint32(rd.buf))
			if size < HeaderSize || size > MaxSize {
				return Msg{}, fmt.Errorf("hangar-sync: a message of %d bytes", size)
			}
			if rd.n >= size {
				b := rd.buf[:size]
				pathLen := int(le.Uint32(b[48:]))
				if HeaderSize+pathLen != size {
					return Msg{}, errors.New("hangar-sync: a message whose path does not fit it")
				}
				m := Msg{
					Op:     le.Uint32(b[4:]),
					ID:     le.Uint64(b[8:]),
					Result: int32(le.Uint32(b[16:])),
					Type:   le.Uint32(b[20:]),
					Flags:  le.Uint32(b[24:]),
					PID:    le.Uint32(b[28:]),
					Start:  le.Uint64(b[32:]),
					End:    le.Uint64(b[40:]),
					Path:   string(b[HeaderSize:]),
				}
				rd.n = copy(rd.buf, rd.buf[size:rd.n])
				return m, nil
			}
		}
		n, err := rd.r.Read(rd.buf[rd.n:])
		rd.n += n
		if err != nil && n == 0 {
			return Msg{}, err
		}
	}
}
