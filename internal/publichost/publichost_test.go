package publichost_test

import (
	"testing"

	"github.com/csnewman/hangar/internal/publichost"
)

func TestNames(t *testing.T) {
	cases := []struct {
		url   string
		style publichost.Style
		label string
		name  string
	}{
		{"https://hangar.example.com", publichost.Subdomain, "registry", "registry.hangar.example.com"},
		{"https://Hangar.Example.com/ignored", publichost.Subdomain, "e-abc", "e-abc.hangar.example.com"},
		{"http://hangar.localhost:8080", publichost.Subdomain, "registry", "registry.hangar.localhost:8080"},
		{"https://hangar1.corp.example.com", publichost.Prefix, "registry", "registry-hangar1.corp.example.com"},
		{"https://hangar1.example.com:8443", publichost.Prefix, "e-abc", "e-abc-hangar1.example.com:8443"},
	}
	for _, c := range cases {
		p, err := publichost.Parse(c.url, c.style)
		if err != nil {
			t.Fatalf("%s: %v", c.url, err)
		}
		if got := p.Name(c.label); got != c.name {
			t.Errorf("%s %s: name for %q is %q, want %q", c.url, c.style, c.label, got, c.name)
		}
		if got, ok := p.Label(c.name); !ok || got != c.label {
			t.Errorf("%s %s: label of %q is %q, %v", c.url, c.style, c.name, got, ok)
		}
	}
}

// A host is one of the names only if it is joined to the public host, port
// included, in the configured style, by a single label.
func TestLabelRefuses(t *testing.T) {
	sub, _ := publichost.Parse("https://hangar.example.com", publichost.Subdomain)
	pre, _ := publichost.Parse("https://hangar.example.com", publichost.Prefix)
	for _, c := range []struct {
		p    *publichost.Public
		host string
	}{
		{sub, "hangar.example.com"},
		{sub, "a.b.hangar.example.com"},
		{sub, "e-x-hangar.example.com"},
		{sub, "e-x.hangar.example.com:8443"},
		{sub, "nothangar.example.com"},
		{pre, "hangar.example.com"},
		{pre, "-hangar.example.com"},
		{pre, "e-x.hangar.example.com"},
		{pre, "a.e-x-hangar.example.com"},
		{pre, "e-x-hangar.example.org"},
	} {
		if label, ok := c.p.Label(c.host); ok {
			t.Errorf("%s %s: took %q as label %q", c.p.Style(), c.host, c.host, label)
		}
	}
	if label, ok := pre.Label("E-X-Hangar.Example.COM"); !ok || label != "e-x" {
		t.Errorf("prefix names are matched whatever their case: %q, %v", label, ok)
	}
}

func TestParse(t *testing.T) {
	for _, c := range []struct {
		url   string
		style publichost.Style
	}{
		{"hangar.example.com", publichost.Subdomain},
		{"ftp://hangar.example.com", publichost.Subdomain},
		{"https://", publichost.Subdomain},
		// A prefix name is a sibling in the host's domain, which a single
		// label or an address does not have.
		{"https://hangar", publichost.Prefix},
		{"https://10.0.0.1", publichost.Prefix},
	} {
		if _, err := publichost.Parse(c.url, c.style); err == nil {
			t.Errorf("%s %s: parsed", c.url, c.style)
		}
	}
	for _, s := range []string{"", "subdomain", "Prefix"} {
		if _, err := publichost.ParseStyle(s); err != nil {
			t.Errorf("style %q: %v", s, err)
		}
	}
	if _, err := publichost.ParseStyle("suffix"); err == nil {
		t.Error("style suffix: parsed")
	}
}
