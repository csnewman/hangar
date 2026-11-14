package logs_test

import (
	"bufio"
	"encoding/json"
	"errors"
	"io"
	"net"
	"testing"

	"github.com/csnewman/hangar/internal/logs"
)

func TestAsk(t *testing.T) {
	answer := func(reply string) io.ReadWriter {
		server, client := net.Pipe()
		go func() {
			defer server.Close()
			line, _ := bufio.NewReader(server).ReadBytes('\n')
			var req logs.Request
			if json.Unmarshal(line, &req) != nil || req.Log != logs.Console || req.Offset != 5 {
				return
			}
			if reply != "" {
				server.Write([]byte(reply + "\n"))
			}
		}()
		return client
	}
	req := logs.Request{Environment: "e1", Log: logs.Console, Offset: 5}
	r, err := logs.Ask(answer(`{"start":5,"size":9,"data":"Ym9vdA=="}`), req)
	if err != nil || r.Start != 5 || string(r.Data) != "boot" {
		t.Fatalf("a part: %+v, %v", r, err)
	}
	if _, err := logs.Ask(answer(`{"error":"no such environment"}`), req); err == nil || err.Error() != "no such environment" {
		t.Fatalf("the worker's error: %v", err)
	}
	if _, err := logs.Ask(answer(""), req); !errors.Is(err, logs.ErrNoReply) {
		t.Fatalf("no answer: %v", err)
	}
}
