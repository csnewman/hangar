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
