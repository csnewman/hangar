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
	got, err := templates.Resolve(tpl, templates.Vars{Name: "BLAH-123-example"})
	if err != nil {
		t.Fatal(err)
	}
	if b := got.Repos[0].Branch; b != "feature/BLAH-123-example" {
		t.Errorf("branch %q", b)
	}
	for _, bad := range []string{"-BLAH-1", "BLAH_1", "BLAH-1-", "blah 1"} {
		if _, err := templates.Resolve(tpl, templates.Vars{Name: bad}); !errors.Is(err, templates.ErrInvalid) {
			t.Errorf("%q: %v, want ErrInvalid", bad, err)
		}
	}
}

// A template's repositories and editor folder are filled in with what is
// known of the environment, and parts of its name its pattern names.
func TestVariables(t *testing.T) {
	tpl, err := templates.Validate(api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024,
		Repos: []api.Repo{{URL: "https://example.com/app.git", Path: "/workspace/{ticket:lower}",
			Ref: "release/{ticket}", Branch: "{owner}/{ticket}-{slug}"}},
		EditorPath: "/workspace/{ticket:lower}/src"},
		NamePattern: "(?P<ticket>[A-Z]+-[0-9]+)(-(?P<slug>[a-z0-9-]+))?"})
	if err != nil {
		t.Fatal(err)
	}
	got, err := templates.Resolve(tpl, templates.Vars{Name: "BLAH-123-example", Owner: "alice", ShortID: "k3x9q2", Template: "app"})
	if err != nil {
		t.Fatal(err)
	}
	r := got.Repos[0]
	if r.Path != "/workspace/blah-123" || r.Ref != "release/BLAH-123" || r.Branch != "alice/BLAH-123-example" ||
		got.EditorPath != "/workspace/blah-123/src" {
		t.Errorf("filled in: %+v, editor %q", r, got.EditorPath)
	}
	// An optional group the name leaves out is empty.
	got, err = templates.Resolve(tpl, templates.Vars{Name: "BLAH-7", Owner: "alice"})
	if err != nil || got.Repos[0].Branch != "alice/BLAH-7-" {
		t.Errorf("without the slug: %q, %v", got.Repos[0].Branch, err)
	}

	short, err := templates.Validate(api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024,
		Repos: []api.Repo{{URL: "https://example.com/app.git", Path: "/workspace/app", Branch: "{template}/{short_id}-{name:upper}"}}}})
	if err != nil {
		t.Fatal(err)
	}
	got, err = templates.Resolve(short, templates.Vars{Name: "fix-it", ShortID: "k3x9q2", Template: "app"})
	if err != nil || got.Repos[0].Branch != "app/k3x9q2-FIX-IT" {
		t.Errorf("built-in variables: %q, %v", got.Repos[0].Branch, err)
	}
}

// What a template cannot fill in is refused when it is saved, not when an
// environment is made from it.
func TestVariablesRefused(t *testing.T) {
	for name, s := range map[string]api.TemplateSpec{
		"an unknown variable":             {Spec: api.Spec{Repos: []api.Repo{{URL: "https://e.com/a.git", Path: "/w", Branch: "{ticket}"}}}},
		"a stray brace":                   {Spec: api.Spec{Repos: []api.Repo{{URL: "https://e.com/a.git", Path: "/w", Branch: "{name"}}}},
		"an unknown modifier":             {Spec: api.Spec{Repos: []api.Repo{{URL: "https://e.com/a.git", Path: "/w", Branch: "{name:title}"}}}},
		"a relative path":                 {Spec: api.Spec{Repos: []api.Repo{{URL: "https://e.com/a.git", Path: "{name}"}}}},
		"an invalid branch":               {Spec: api.Spec{Repos: []api.Repo{{URL: "https://e.com/a.git", Path: "/w", Branch: "{name}..x"}}}},
		"a group named like a variable":   {Spec: api.Spec{}, NamePattern: "(?P<owner>[a-z]+)"},
		"a variable in the editor folder": {Spec: api.Spec{EditorPath: "/w/{nope}"}},
	} {
		s.Image, s.CPUs, s.MemoryMiB = "img", 1, 1024
		if _, err := templates.Validate(s); !errors.Is(err, templates.ErrInvalid) {
			t.Errorf("%s: %v, want ErrInvalid", name, err)
		}
	}
}
