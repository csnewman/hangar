package audit_test

import (
	"context"
	"errors"
	"slices"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/placement"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
	"github.com/csnewman/hangar/internal/workers"
)

func actions(entries []audit.Entry) []string {
	var out []string
	for _, e := range entries {
		out = append(out, e.Action+" by "+e.ActorKind+":"+e.ActorName)
	}
	return out
}

// Every step of an environment's life is recorded, by whoever took it: the
// person who asked, Hangar deciding where it runs, the worker running it.
func TestEnvironmentHistory(t *testing.T) {
	ctx := context.Background()
	d := dbtest.Open(t)
	log := audit.NewLog(d)
	um := users.NewManager(d)
	wm := workers.NewManager(d)
	em := environments.NewManager(d)

	admin := audit.WithActor(ctx, audit.User(audit.SystemHangar, "hangar"))
	alice, err := um.Create(admin, users.NewUser{Username: "alice", Password: "password1"})
	if err != nil {
		t.Fatal(err)
	}
	asAlice := audit.WithActor(ctx, audit.Actor{UserID: alice.ID, Name: "alice", IP: "203.0.113.7"})
	p := users.Principal{UserID: alice.ID, Username: "alice"}
	tmpl, err := templates.NewManager(d).Create(asAlice, p, templates.Input{
		Name: "t", Spec: api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024}},
	})
	if err != nil {
		t.Fatal(err)
	}
	env, err := em.Create(asAlice, p, api.CreateEnvironment{TemplateID: tmpl.ID, Name: "e"})
	if err != nil {
		t.Fatal(err)
	}
	cred, _ := wm.Register(ctx, api.RegisterWorker{Name: "w"})
	if err := wm.ReportStatus(ctx, cred.ID, api.WorkerStatus{Capacity: api.Resources{CPUs: 4, MemoryMiB: 8192}}); err != nil {
		t.Fatal(err)
	}
	if n, err := placement.NewManager(d).Place(ctx); err != nil || n != 1 {
		t.Fatalf("Place = %d, %v", n, err)
	}
	if err := wm.ReportStatus(ctx, cred.ID, api.WorkerStatus{Capacity: api.Resources{CPUs: 4, MemoryMiB: 8192},
		Environments: []api.ObservedEnvironment{{ID: env.ID, Phase: api.PhaseRunning}}}); err != nil {
		t.Fatal(err)
	}
	if _, err := em.SetDesired(asAlice, p, env.ID, api.DesiredStopped); err != nil {
		t.Fatal(err)
	}

	entries, err := log.List(ctx, audit.Query{Subjects: []string{"environment:" + env.ID}})
	if err != nil {
		t.Fatal(err)
	}
	got := actions(entries)
	slices.Reverse(got)
	want := []string{
		"environment.create by person:alice",
		"environment.place by system:hangar:placement",
		"environment.phase by worker:w",
		"environment.stop by person:alice",
	}
	if !slices.Equal(got, want) {
		t.Fatalf("the environment's history is\n%q\nwant\n%q", got, want)
	}
	if entries[len(entries)-1].IP != "203.0.113.7" {
		t.Errorf("the request's address was not kept: %q", entries[len(entries)-1].IP)
	}

	// The same events are the template's and the image's where they
	// concern them, and alice's as owner.
	byTemplate, _ := log.List(ctx, audit.Query{Subjects: []string{"template:" + tmpl.ID}})
	if len(byTemplate) != 2 { // the template made, and an environment made from it
		t.Errorf("the template's history: %q", actions(byTemplate))
	}
	byImage, _ := log.List(ctx, audit.Query{Subjects: []string{"image:img"}})
	if len(byImage) < 2 {
		t.Errorf("the image's history: %q", actions(byImage))
	}
	mine, _ := log.List(ctx, audit.Query{AnyOf: []string{"owner:" + alice.ID}})
	if len(mine) < 5 {
		t.Errorf("alice sees %q", actions(mine))
	}
}

// A failed sign-in is recorded by nobody, under the name that was tried; a
// successful one by the person.
func TestSignIns(t *testing.T) {
	ctx := context.Background()
	d := dbtest.Open(t)
	log := audit.NewLog(d)
	um := users.NewManager(d)
	u, err := um.Create(ctx, users.NewUser{Username: "bob", Password: "password1"})
	if err != nil {
		t.Fatal(err)
	}
	from := audit.WithActor(ctx, audit.Actor{IP: "198.51.100.1"})
	if _, _, err := um.Login(from, "bob", "wrong-password"); !errors.Is(err, users.ErrBadCredentials) {
		t.Fatal(err)
	}
	if _, _, err := um.Login(from, "bob", "password1"); err != nil {
		t.Fatal(err)
	}
	// System users are nobody to sign in as.
	if _, _, err := um.Login(from, "hangar", ""); !errors.Is(err, users.ErrBadCredentials) {
		t.Fatalf("signing in as a system user: %v", err)
	}
	entries, _ := log.List(ctx, audit.Query{Subjects: []string{"user:" + u.ID}})
	got := actions(entries)
	slices.Reverse(got)
	want := []string{"user.create by system:hangar", "auth.login_failed by anonymous:bob", "auth.login by person:bob"}
	if !slices.Equal(got, want) {
		t.Fatalf("bob's history is %q, want %q", got, want)
	}

	list, _ := um.List(ctx)
	for _, x := range list {
		if x.Username == "hangar" || x.Username == "hangar:placement" {
			t.Errorf("a system user is listed among people: %s", x.Username)
		}
	}
}
