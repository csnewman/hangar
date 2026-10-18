package vm

import (
	"context"
	"io"
	"os"

	"github.com/containers/gvisor-tap-vsock/pkg/transport"
	"github.com/containers/gvisor-tap-vsock/pkg/types"
	"github.com/containers/gvisor-tap-vsock/pkg/virtualnetwork"
)

// guestMAC is the address of the guest's network device, which the network
// leases guestIP to.
const (
	guestMAC = "5a:94:ef:e4:0c:ee"
	guestIP  = "192.168.127.2"
)

// startNetwork serves a guest's network from inside the worker: a TCP/IP
// stack in user space (gvisor-tap-vsock) that answers the guest's DHCP and
// DNS and makes its connections out through the host's own sockets. The
// guest's device reaches it over socket, a unix datagram socket libkrun
// sends its frames to. Nothing on the host's loopback is reachable from it:
// a guest talks to the worker over vsock.
func startNetwork(ctx context.Context, socket string) (io.Closer, error) {
	vn, err := virtualnetwork.New(&types.Configuration{
		MTU:               1500,
		Subnet:            "192.168.127.0/24",
		GatewayIP:         "192.168.127.1",
		GatewayMacAddress: "5a:94:ef:e4:0c:dd",
		DHCPStaticLeases:  map[string]string{guestIP: guestMAC},
		Protocol:          types.VfkitProtocol,
	})
	if err != nil {
		return nil, err
	}
	os.Remove(socket)
	conn, err := transport.ListenUnixgram("unixgram://" + socket)
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithCancel(ctx)
	go func() {
		vc, err := transport.AcceptVfkit(conn)
		if err != nil {
			return
		}
		vn.AcceptVfkit(ctx, vc)
	}()
	return closer(func() {
		cancel()
		conn.Close()
		os.Remove(socket)
	}), nil
}
