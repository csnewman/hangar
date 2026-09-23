// Package webui holds the built web UI, embedded into hangar-server so the
// control plane is one binary with no Node.js process beside it.
//
// The UI is built by `npm run build` in web/, which writes into dist/app. A
// binary built without that step has no UI and says so.
package webui

import (
	"embed"
	"io/fs"
)

//go:embed all:dist
var dist embed.FS

// FS returns the built UI, or nil if it was not built before this binary.
func FS() fs.FS {
	app, err := fs.Sub(dist, "dist/app")
	if err != nil {
		return nil
	}
	if _, err := fs.Stat(app, "index.html"); err != nil {
		return nil
	}
	return app
}
