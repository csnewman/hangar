package environments_test

import (
	"errors"
	"fmt"
	"regexp"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
)

// Every environment is given a short ID of its own, which its hosts are
// named with.
func TestShortIDs(t *testing.T) {
	d := dbtest.Open(t)
	em, tm := environments.NewManager(d), templates.NewManager(d)
	u, _ := users.NewManager(d).Create(ctx, users.NewUser{Username: "owner", Password: "password1"})
	p := users.Principal{UserID: u.ID, Username: u.Username}
	tpl, err := tm.Create(ctx, p, templates.Input{Name: "t",
		Spec: api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024}}})
	if err != nil {
		t.Fatal(err)
	}
	format := regexp.MustCompile(`^[a-z0-9]{6}$`)
	seen := map[string]bool{}
	for i := range 50 {
		e, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tpl.ID, Name: fmt.Sprintf("e%d", i)})
		if err != nil {
			t.Fatal(err)
		}
		if !format.MatchString(e.ShortID) || seen[e.ShortID] {
			t.Fatalf("environment %d: short ID %q", i, e.ShortID)
		}
		seen[e.ShortID] = true
		got, err := em.Get(ctx, p, e.ID)
		if err != nil || got.ShortID != e.ShortID {
			t.Fatalf("read back: %q, %v", got.ShortID, err)
		}
	}
}

// A name keeps its case, but two of one owner's cannot differ only in it,
// and one is found whatever case it is asked for in.
func TestNameCase(t *testing.T) {
	d := dbtest.Open(t)
	em, tm := environments.NewManager(d), templates.NewManager(d)
	u, _ := users.NewManager(d).Create(ctx, users.NewUser{Username: "owner", Password: "password1"})
	p := users.Principal{UserID: u.ID, Username: u.Username}
	tpl, err := tm.Create(ctx, p, templates.Input{Name: "t",
		Spec: api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024}}})
	if err != nil {
		t.Fatal(err)
	}
	e, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tpl.ID, Name: "BLAH-123-example"})
	if err != nil {
		t.Fatal(err)
	}
	if e.Name != "BLAH-123-example" {
		t.Errorf("name %q", e.Name)
	}
	if _, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tpl.ID, Name: "blah-123-example"}); !errors.Is(err, environments.ErrConflict) {
		t.Errorf("the same name in lower case: %v, want ErrConflict", err)
	}
	got, err := em.ByName(ctx, p, "owner", "blah-123-EXAMPLE")
	if err != nil || got.ID != e.ID {
		t.Errorf("found in another case: %v, %v", got.ID, err)
	}
}

// A template may use the short ID an environment is given, and its owner.
func TestShortIDInTemplate(t *testing.T) {
	d := dbtest.Open(t)
	em, tm := environments.NewManager(d), templates.NewManager(d)
	u, _ := users.NewManager(d).Create(ctx, users.NewUser{Username: "owner", Password: "password1"})
	p := users.Principal{UserID: u.ID}
	tpl, err := tm.Create(ctx, users.Principal{UserID: u.ID, Username: u.Username}, templates.Input{Name: "t",
		Spec: api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024,
			Repos: []api.Repo{{URL: "https://example.com/a.git", Path: "/workspace/a", Branch: "{owner}/{short_id}"}}}}})
	if err != nil {
		t.Fatal(err)
	}
	e, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tpl.ID, Name: "e"})
	if err != nil {
		t.Fatal(err)
	}
	if want := "owner/" + e.ShortID; e.Spec.Repos[0].Branch != want {
		t.Errorf("branch %q, want %q", e.Spec.Repos[0].Branch, want)
	}
	if len(e.TemplateChanges) != 0 {
		t.Errorf("an environment just made differs from its template in %v", e.TemplateChanges)
	}
}
