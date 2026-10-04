package hangarsync_test

import (
	"bytes"
	"io"
	"testing"

	"github.com/csnewman/hangar/internal/hangarsync"
)

// oneByte reads a byte at a time, as the most split-up stream could.
type oneByte struct{ r io.Reader }

func (o oneByte) Read(p []byte) (int, error) { return o.r.Read(p[:1]) }

func TestRoundTrip(t *testing.T) {
	msgs := []hangarsync.Msg{
		{Op: hangarsync.OpLock, ID: 7, Type: 1, Flags: hangarsync.FlagWait, PID: 42, Start: 0, End: 1<<63 - 1, Path: "/home/dev/proj/db.sqlite"},
		{Op: hangarsync.OpIdle, Path: "/home/dev/proj/db.sqlite"},
		{Op: hangarsync.OpReply, ID: 7, Result: -11},
		{Op: hangarsync.OpClearPaths},
	}
	var stream bytes.Buffer
	for _, m := range msgs {
		stream.Write(m.Marshal())
	}
	for name, r := range map[string]io.Reader{
		"joined": bytes.NewReader(stream.Bytes()),
		"split":  oneByte{bytes.NewReader(stream.Bytes())},
	} {
		rd := hangarsync.NewReader(r)
		for i, want := range msgs {
			got, err := rd.Next()
			if err != nil {
				t.Fatalf("%s: message %d: %v", name, i, err)
			}
			if got != want {
				t.Fatalf("%s: message %d: got %+v, want %+v", name, i, got, want)
			}
		}
		if _, err := rd.Next(); err != io.EOF {
			t.Fatalf("%s: after the last: %v", name, err)
		}
	}
}

func TestHeaderSize(t *testing.T) {
	if n := len(hangarsync.Msg{}.Marshal()); n != hangarsync.HeaderSize {
		t.Fatalf("an empty message is %d bytes, want %d", n, hangarsync.HeaderSize)
	}
}
