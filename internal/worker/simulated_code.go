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
	"github.com/csnewman/hangar/internal/code"
)

// DialCode connects to a running environment's Code tab service. There is no
// guest to run hangar-code in, so a stand-in here answers the requests that
// need no files -- hello, and an empty listing -- in the service's framing:
// enough to show the path through the control plane and tunnel works.
func (s *Simulated) DialCode(_ context.Context, id string) (net.Conn, error) {
	s.mu.Lock()
	e, ok := s.envs[id]
	running := ok && e.phase == api.PhaseRunning
	s.mu.Unlock()
	if !running {
		return nil, fmt.Errorf("environment %s is not running here", id)
	}
	a, b := net.Pipe()
	go func() {
		defer b.Close()
		serveSimulatedCode(b)
	}()
	return a, nil
}

func serveSimulatedCode(conn io.ReadWriter) {
	br := bufio.NewReader(conn)
	line, err := br.ReadBytes('\n')
	if err != nil {
		return
	}
	var req code.Request
	if json.Unmarshal(line, &req) != nil {
		return
	}
	root := req.Root
	if root == "" {
		root = "/home/dev"
	}
	var hdr [4]byte
	for {
		if _, err := io.ReadFull(br, hdr[:]); err != nil {
			return
		}
		frame := make([]byte, binary.BigEndian.Uint32(hdr[:]))
		if _, err := io.ReadFull(br, frame); err != nil {
			return
		}
		if len(frame) == 0 || frame[0] != 'r' {
			continue
		}
		var call struct {
			ID     int    `json:"id"`
			Method string `json:"method"`
		}
		if json.Unmarshal(frame[1:], &call) != nil {
			continue
		}
		reply := map[string]any{"id": call.ID}
		switch call.Method {
		case "hello":
			reply["result"] = map[string]any{"root": root, "simulated": true}
		case "fs.list":
			reply["result"] = []any{}
		case "git.status":
			reply["result"] = map[string]any{"repo": false, "changes": []any{}}
		default:
			reply["error"] = "a simulated environment has no files"
		}
		out, _ := json.Marshal(reply)
		msg := append([]byte{'r'}, out...)
		binary.BigEndian.PutUint32(hdr[:], uint32(len(msg)))
		if _, err := conn.Write(append(hdr[:], msg...)); err != nil {
			return
		}
	}
}
