package ch

import "testing"

func TestSelected(t *testing.T) {
	cases := []struct {
		in   string
		want string
	}{
		{"always within_size advise [never] deny force\n", "never"},
		{"always within_size [advise] never deny force\n", "advise"},
		{"[always] within_size advise never deny force\n", "always"},
		{"always within_size advise never deny [force]\n", "force"},
		// A kernel that prints no brackets tells us nothing, and guessing
		// would be worse than saying so.
		{"always never\n", ""},
		{"", ""},
	}
	for _, c := range cases {
		if got := selected(c.in); got != c.want {
			t.Errorf("selected(%q) = %q, want %q", c.in, got, c.want)
		}
	}
}
