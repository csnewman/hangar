package environments_test

import (
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
)

// An environment's phase is what its worker last reported; whether that
// worker is still reporting is said beside it.
func TestWorkerOnline(t *testing.T) {
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
	set := func(sql string, args ...any) {
		t.Helper()
		if err := d.Transact(ctx, func(tx db.Tx) error { _, err := tx.Exec(ctx, sql, args...); return err }); err != nil {
			t.Fatal(err)
		}
	}
	online := func() bool {
		t.Helper()
		e, err := em.Get(ctx, p, env.ID)
		if err != nil {
			t.Fatal(err)
		}
		return e.WorkerOnline
	}
	if online() {
		t.Error("an environment on no worker has its worker online")
	}

	var worker string
	if err := d.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `INSERT INTO workers (name, credential_hash, last_seen_at)
			VALUES ('w', '\x00', now()) RETURNING id`).Scan(&worker)
	}); err != nil {
		t.Fatal(err)
	}
	set(`UPDATE environments SET worker_id = $2, phase = 'running' WHERE id = $1`, env.ID, worker)
	if !online() {
		t.Error("a worker that just reported is not online")
	}
	set(`UPDATE workers SET last_seen_at = now() - interval '10 minutes' WHERE id = $1`, worker)
	if online() {
		t.Error("a worker silent for ten minutes is online")
	}
	set(`UPDATE workers SET last_seen_at = now(), revoked_at = now() WHERE id = $1`, worker)
	if online() {
		t.Error("a revoked worker is online")
	}
	if e, _ := em.Get(ctx, p, env.ID); e.Phase != api.PhaseRunning {
		t.Errorf("the phase changed with the worker: %s", e.Phase)
	}
}
