//go:build !darwin

package vm

import (
	"context"
	"fmt"
	"io"
)

// guestMAC is the address of the guest's network device under libkrun.
const guestMAC = "5a:94:ef:e4:0c:ee"

// startNetwork is macOS's: on Linux passt carries the guest's network.
func startNetwork(context.Context, string) (io.Closer, error) {
	return nil, fmt.Errorf("a network in the worker is %w", errUnsupported)
}
