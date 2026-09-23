package worker

import (
	"bufio"
	"context"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"net"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/desktop"
)

// DialDesktop connects to a running environment's desktop. There is no guest
// to run one in, so the desktop is a test card served by a VNC server here:
// enough for a viewer to connect, draw it and send input that goes nowhere,
// which shows the path through the control plane and tunnel works.
func (s *Simulated) DialDesktop(_ context.Context, id string) (net.Conn, error) {
	s.mu.Lock()
	e, ok := s.envs[id]
	running := ok && e.phase == api.PhaseRunning
	hasDesktop := ok && e.spec.Display != api.DisplayNone
	s.mu.Unlock()
	if !running {
		return nil, fmt.Errorf("environment %s is not running here", id)
	}
	if !hasDesktop {
		return nil, fmt.Errorf("environment %s is headless", id)
	}
	a, b := net.Pipe()
	go func() {
		defer b.Close()
		br := bufio.NewReader(b)
		line, err := br.ReadBytes('\n')
		if err != nil {
			return
		}
		var req desktop.Request
		if json.Unmarshal(line, &req) != nil {
			return
		}
		switch req.Op {
		case desktop.OpVNC:
			serveTestCard(struct {
				io.Reader
				io.Writer
			}{br, b})
		case desktop.OpResize:
			// The card has one size; the request is accepted and changes
			// nothing.
			b.Write([]byte("{}\n"))
		}
	}()
	return a, nil
}

const (
	testCardWidth  = 1024
	testCardHeight = 640
	// SimulatedDesktopName is the desktop name the test card's server gives.
	SimulatedDesktopName = "Hangar simulated desktop"
)

// pixelFormat is the part of an RFB pixel format a true-colour server needs.
type pixelFormat struct {
	bpp       uint8
	bigEndian bool
	rMax      uint16
	gMax      uint16
	bMax      uint16
	rShift    uint8
	gShift    uint8
	bShift    uint8
}

// serveTestCard speaks RFB 3.8 with no authentication and raw encoding:
// the least a viewer needs. Input is read and ignored.
func serveTestCard(conn io.ReadWriter) error {
	r := bufio.NewReader(conn)
	if _, err := io.WriteString(conn, "RFB 003.008\n"); err != nil {
		return err
	}
	version := make([]byte, 12)
	if _, err := io.ReadFull(r, version); err != nil {
		return err
	}
	// One security type, None.
	if _, err := conn.Write([]byte{1, 1}); err != nil {
		return err
	}
	if _, err := r.ReadByte(); err != nil {
		return err
	}
	if _, err := conn.Write([]byte{0, 0, 0, 0}); err != nil { // SecurityResult: OK
		return err
	}
	if _, err := r.ReadByte(); err != nil { // ClientInit: shared flag
		return err
	}

	pf := pixelFormat{bpp: 32, rMax: 255, gMax: 255, bMax: 255, rShift: 16, gShift: 8, bShift: 0}
	init := make([]byte, 0, 24+len(SimulatedDesktopName))
	init = binary.BigEndian.AppendUint16(init, testCardWidth)
	init = binary.BigEndian.AppendUint16(init, testCardHeight)
	init = append(init, 32, 24, 0, 1)
	init = binary.BigEndian.AppendUint16(init, 255)
	init = binary.BigEndian.AppendUint16(init, 255)
	init = binary.BigEndian.AppendUint16(init, 255)
	init = append(init, 16, 8, 0, 0, 0, 0)
	init = binary.BigEndian.AppendUint32(init, uint32(len(SimulatedDesktopName)))
	init = append(init, SimulatedDesktopName...)
	if _, err := conn.Write(init); err != nil {
		return err
	}

	for {
		typ, err := r.ReadByte()
		if err != nil {
			return err
		}
		switch typ {
		case 0: // SetPixelFormat
			b := make([]byte, 19)
			if _, err := io.ReadFull(r, b); err != nil {
				return err
			}
			p := b[3:]
			pf = pixelFormat{
				bpp: p[0], bigEndian: p[2] != 0,
				rMax: binary.BigEndian.Uint16(p[4:]), gMax: binary.BigEndian.Uint16(p[6:]), bMax: binary.BigEndian.Uint16(p[8:]),
				rShift: p[10], gShift: p[11], bShift: p[12],
			}
		case 2: // SetEncodings
			b := make([]byte, 3)
			if _, err := io.ReadFull(r, b); err != nil {
				return err
			}
			n := int(binary.BigEndian.Uint16(b[1:]))
			if _, err := io.CopyN(io.Discard, r, int64(4*n)); err != nil {
				return err
			}
		case 3: // FramebufferUpdateRequest
			b := make([]byte, 9)
			if _, err := io.ReadFull(r, b); err != nil {
				return err
			}
			// The card never changes, so only a full request is answered.
			if b[0] == 0 {
				if _, err := conn.Write(testCardFrame(pf)); err != nil {
					return err
				}
			}
		case 4: // KeyEvent
			if _, err := io.CopyN(io.Discard, r, 7); err != nil {
				return err
			}
		case 5: // PointerEvent
			if _, err := io.CopyN(io.Discard, r, 5); err != nil {
				return err
			}
		case 6: // ClientCutText
			b := make([]byte, 7)
			if _, err := io.ReadFull(r, b); err != nil {
				return err
			}
			if _, err := io.CopyN(io.Discard, r, int64(binary.BigEndian.Uint32(b[3:]))); err != nil {
				return err
			}
		default:
			return fmt.Errorf("unsupported RFB client message %d", typ)
		}
	}
}

// testCardFrame is one FramebufferUpdate carrying the whole card, raw, in
// the viewer's pixel format: eight colour bars over a grey ramp.
func testCardFrame(pf pixelFormat) []byte {
	bytesPP := int(pf.bpp) / 8
	out := make([]byte, 0, 16+testCardWidth*testCardHeight*bytesPP)
	out = append(out, 0, 0) // FramebufferUpdate, padding
	out = binary.BigEndian.AppendUint16(out, 1)
	out = binary.BigEndian.AppendUint16(out, 0)
	out = binary.BigEndian.AppendUint16(out, 0)
	out = binary.BigEndian.AppendUint16(out, testCardWidth)
	out = binary.BigEndian.AppendUint16(out, testCardHeight)
	out = binary.BigEndian.AppendUint32(out, 0) // raw
	bars := [8][3]uint32{{235, 235, 235}, {235, 235, 16}, {16, 235, 235}, {16, 235, 16},
		{235, 16, 235}, {235, 16, 16}, {16, 16, 235}, {16, 16, 16}}
	px := make([]byte, 4)
	for y := 0; y < testCardHeight; y++ {
		for x := 0; x < testCardWidth; x++ {
			var c [3]uint32
			if y < testCardHeight*3/4 {
				c = bars[x*8/testCardWidth]
			} else {
				g := uint32(x * 255 / (testCardWidth - 1))
				c = [3]uint32{g, g, g}
			}
			v := (c[0]*uint32(pf.rMax)/255)<<pf.rShift | (c[1]*uint32(pf.gMax)/255)<<pf.gShift | (c[2]*uint32(pf.bMax)/255)<<pf.bShift
			switch bytesPP {
			case 4:
				if pf.bigEndian {
					binary.BigEndian.PutUint32(px, v)
				} else {
					binary.LittleEndian.PutUint32(px, v)
				}
			case 2:
				if pf.bigEndian {
					binary.BigEndian.PutUint16(px, uint16(v))
				} else {
					binary.LittleEndian.PutUint16(px, uint16(v))
				}
			default:
				px[0] = byte(v)
			}
			out = append(out, px[:bytesPP]...)
		}
	}
	return out
}
