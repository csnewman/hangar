package agent_test

import (
	"encoding/json"
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/agent"
	"github.com/csnewman/hangar/internal/vsock"
)

func TestAcceptWaitsPastAConnectionThatEndsBeforeItsHello(t *testing.T) {
	// Short, under macOS's 104-byte limit on socket paths.
	dir, err := os.MkdirTemp("/tmp", "agent")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	base := filepath.Join(dir, "v")
	srv, err := agent.ListenHybrid(base, 7)
	if err != nil {
		t.Fatal(err)
	}
	defer srv.Close()

	go func() {
		path := vsock.HybridPath(base, agent.Port)
		if c, err := net.Dial("unix", path); err == nil {
			c.Close()
		}
		c, err := net.Dial("unix", path)
		if err != nil {
			return
		}
		defer c.Close()
		hello, _ := json.Marshal(agent.Hello{Kind: agent.KindHello, Version: agent.ProtocolVersion, Hostname: "guest"})
		c.Write(append(hello, '\n'))
		time.Sleep(time.Second)
	}()

	sess, err := srv.Accept(5 * time.Second)
	if err != nil {
		t.Fatalf("Accept: %v", err)
	}
	if sess.Hello.Hostname != "guest" || sess.CID != 7 {
		t.Fatalf("got hello %+v from CID %d", sess.Hello, sess.CID)
	}
}
