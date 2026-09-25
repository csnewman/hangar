// Package version names this build of Hangar.
package version

import "runtime/debug"

// Version is set when the binary is linked:
//
//	go build -ldflags "-X github.com/csnewman/hangar/internal/version.Version=v1.2.3"
//
// Left unset, String falls back to the commit Go recorded, then "dev".
var Version string

// String is this build's version.
func String() string {
	if Version != "" {
		return Version
	}
	if info, ok := debug.ReadBuildInfo(); ok {
		rev, dirty := "", false
		for _, s := range info.Settings {
			switch s.Key {
			case "vcs.revision":
				rev = s.Value
			case "vcs.modified":
				dirty = s.Value == "true"
			}
		}
		if len(rev) > 12 {
			rev = rev[:12]
		}
		if rev != "" {
			if dirty {
				rev += "-dirty"
			}
			return rev
		}
	}
	return "dev"
}
