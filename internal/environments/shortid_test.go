package environments_test

import (
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
