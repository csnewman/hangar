package terminal_test

import (
	"bytes"
	"testing"

	"github.com/csnewman/hangar/internal/terminal"
)

func TestFrames(t *testing.T) {
	var b bytes.Buffer
	if err := terminal.WriteFrame(&b, terminal.FrameResize, terminal.ResizePayload(132, 43)); err != nil {
		t.Fatal(err)
	}
	typ, p, err := terminal.ReadFrame(&b)
	if err != nil || typ != terminal.FrameResize {
		t.Fatalf("read: %c %v", typ, err)
	}
	cols, rows, err := terminal.ParseResize(p)
	if err != nil || cols != 132 || rows != 43 {
		t.Fatalf("resize %dx%d %v", cols, rows, err)
	}
	gotCols, gotRows, n, owner, err := terminal.ParseSize(terminal.SizePayload(80, 24, 3, true))
	if err != nil || gotCols != 80 || gotRows != 24 || n != 3 || !owner {
		t.Fatalf("size %dx%d %d %v %v", gotCols, gotRows, n, owner, err)
	}
	if err := terminal.WriteFrame(&b, terminal.FrameInput, make([]byte, terminal.MaxFrame+1)); err == nil {
		t.Fatal("wrote an oversized frame")
	}
}
