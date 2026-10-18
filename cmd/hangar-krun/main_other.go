//go:build !(darwin && krun)

// Command hangar-krun runs one machine under libkrun, from the JSON
// description the worker writes (krun.Config). This build has no libkrun:
// dev/krun-build.sh builds the one that does.
package main

import (
	"fmt"
	"os"
)

func main() {
	fmt.Fprintln(os.Stderr, "hangar-krun runs machines under libkrun on macOS: build it with dev/krun-build.sh")
	os.Exit(1)
}
