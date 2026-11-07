package api_test

import (
	"slices"
	"testing"

	"github.com/csnewman/hangar/internal/api"
)

func TestCodeTrust(t *testing.T) {
	for _, c := range []struct {
		name string
		spec api.Spec
		want []string
	}{
		{"repositories and projects", api.Spec{
			Repos:          []api.Repo{{Path: "/home/dev/app"}},
			Projects:       []string{"/home/dev/app", "/home/dev/docs"},
			TrustedFolders: []string{"/opt/tools"},
		}, []string{"/home/dev/app", "/home/dev/docs", "/opt/tools"}},
		{"only an editor folder", api.Spec{EditorPath: "/home/dev/work"}, []string{"/home/dev/work"}},
		{"nothing named: the home folder", api.Spec{}, []string{"~"}},
		{"trusting nothing", api.Spec{
			Repos:  []api.Repo{{Path: "/home/dev/app"}},
			Access: api.Untrusted,
		}, []string{}},
	} {
		if got := c.spec.CodeTrust(); !slices.Equal(got, c.want) {
			t.Errorf("%s: %q, want %q", c.name, got, c.want)
		}
	}
}
