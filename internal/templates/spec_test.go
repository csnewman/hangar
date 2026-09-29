package templates_test

import (
	"errors"
	"slices"
	"strings"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/templates"
)

func spec(names ...string) api.TemplateSpec {
	return api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024, WebNames: names}}
}

// A web server's name goes in front of -env<short> in one DNS label, written
// as the user would, with or without the hyphen.
func TestWebNames(t *testing.T) {
	got, err := templates.Validate(spec(" Dashboard- ", "customer-ui", "api2"))
	if err != nil {
		t.Fatal(err)
	}
	if want := []string{"dashboard", "customer-ui", "api2"}; !slices.Equal(got.WebNames, want) {
		t.Errorf("names %v, want %v", got.WebNames, want)
	}
	for _, bad := range [][]string{
		{"has space"},
		{"-leading"},
		{"under_score"},
		{strings.Repeat("a", templates.MaxWebName+1)},
		{"twice", "twice-"},
	} {
		if _, err := templates.Validate(spec(bad...)); !errors.Is(err, templates.ErrInvalid) {
			t.Errorf("%q: %v, want ErrInvalid", bad, err)
		}
	}
	if _, err := templates.Validate(spec(strings.Repeat("a", templates.MaxWebName))); err != nil {
		t.Errorf("the longest name: %v", err)
	}
}

// A name may be in either case, like the ticket numbers names often are, and
// branch names made from it keep its case.
func TestNameCase(t *testing.T) {
	tpl := api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024,
		Repos: []api.Repo{{URL: "https://example.com/r.git", Path: "/workspace/r", Branch: "feature/{name}"}}},
		NamePattern: "[A-Z]+-[0-9]+(-[a-z0-9-]+)?"}
	got, err := templates.Resolve(tpl, "BLAH-123-example")
	if err != nil {
		t.Fatal(err)
	}
	if b := got.Repos[0].Branch; b != "feature/BLAH-123-example" {
		t.Errorf("branch %q", b)
	}
	for _, bad := range []string{"-BLAH-1", "BLAH_1", "BLAH-1-", "blah 1"} {
		if _, err := templates.Resolve(tpl, bad); !errors.Is(err, templates.ErrInvalid) {
			t.Errorf("%q: %v, want ErrInvalid", bad, err)
		}
	}
}
