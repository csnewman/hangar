package environments_test

import (
	"errors"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
)

// An upgrade waits for the worker to have compared the environment's
// writable layer with the newer image, and for nothing to be hidden by it,
// unless forced; it and a rollback happen at the next start, and keeping an
// upgrade drops its rollback.
func TestImageUpgrade(t *testing.T) {
	d := dbtest.Open(t)
	em, tm := environments.NewManager(d), templates.NewManager(d)
	u, _ := users.NewManager(d).Create(ctx, users.NewUser{Username: "owner", Password: "password1"})
	p := users.Principal{UserID: u.ID, Username: u.Username}
	tpl, err := tm.Create(ctx, p, templates.Input{Name: "t",
		Spec: api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 2, MemoryMiB: 2048, Display: api.DisplayNone}}})
	if err != nil {
		t.Fatal(err)
	}
	env, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tpl.ID, Name: "e"})
	if err != nil {
		t.Fatal(err)
	}
	// As its worker would report it.
	report := func(update, rollback string) {
		t.Helper()
		if err := d.Transact(ctx, func(tx db.Tx) error {
			_, err := tx.Exec(ctx, `UPDATE environments SET image_digest = 'old', image_update = $2::jsonb,
				image_rollback = $3::jsonb WHERE id = $1`, env.ID, nullable(update), nullable(rollback))
			return err
		}); err != nil {
			t.Fatal(err)
		}
	}
	expectConflict := func(what string, err error) {
		t.Helper()
		if !errors.Is(err, environments.ErrConflict) {
			t.Errorf("%s: %v, want ErrConflict", what, err)
		}
	}

	_, err = em.UpgradeImage(ctx, p, env.ID, false)
	expectConflict("upgrading with no newer copy", err)

	report(`{"digest": "new", "checked": false}`, "")
	_, err = em.UpgradeImage(ctx, p, env.ID, false)
	expectConflict("upgrading before the check", err)

	report(`{"digest": "new", "checked": true, "conflicts": ["/var/lib/dpkg/status"], "packages": true}`, "")
	_, err = em.UpgradeImage(ctx, p, env.ID, false)
	expectConflict("upgrading over installed packages", err)
	got, err := em.UpgradeImage(ctx, p, env.ID, true)
	if err != nil {
		t.Fatal(err)
	}
	if got.ImageChange != api.ImageChangeUpgrade {
		t.Fatalf("after forcing the upgrade: change %q", got.ImageChange)
	}
	if got, err = em.CancelImageChange(ctx, p, env.ID); err != nil || got.ImageChange != "" {
		t.Fatalf("cancelling: change %q, %v", got.ImageChange, err)
	}

	report(`{"digest": "new", "checked": true}`, "")
	if got, err = em.UpgradeImage(ctx, p, env.ID, false); err != nil || got.ImageChange != api.ImageChangeUpgrade {
		t.Fatalf("upgrading with nothing hidden: change %q, %v", got.ImageChange, err)
	}

	// Upgraded, and keeping a rollback to the copy it had.
	report("", `{"digest": "older", "at": "2026-09-27T00:00:00Z", "size_bytes": 1}`)
	if _, err := em.KeepImage(ctx, p, env.ID); err != nil {
		t.Fatal(err)
	}
	got, err = em.RollbackImage(ctx, p, env.ID)
	if err != nil || got.ImageChange != api.ImageChangeRollback {
		t.Fatalf("rolling back: change %q, %v", got.ImageChange, err)
	}

	report("", "")
	_, err = em.RollbackImage(ctx, p, env.ID)
	expectConflict("rolling back with no rollback", err)
	_, err = em.KeepImage(ctx, p, env.ID)
	expectConflict("keeping with no rollback", err)
}

func nullable(s string) *string {
	if s == "" {
		return nil
	}
	return &s
}
