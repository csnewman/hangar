//go:build !(darwin && krun)

package main

import (
	"fmt"
	"os"
)

func main() {
	fmt.Fprintln(os.Stderr, "hangar-krun runs machines under libkrun on macOS: build it with dev/krun-build.sh")
	os.Exit(1)
}
