package teams_test

import (
	"context"
	"errors"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/teams"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
)

var ctx = context.Background()

type world struct {
	users     *users.Manager
	teams     *teams.Manager
	templates *templates.Manager
	admin     users.Principal
}

func open(t *testing.T) (*world, *db.DB) {
	t.Helper()
	d := dbtest.Open(t)
	w := &world{users: users.NewManager(d), teams: teams.NewManager(d), templates: templates.NewManager(d)}
	w.admin = w.person(t, "root", true)
	return w, d
}

func (w *world) person(t *testing.T, name string, admin bool) users.Principal {
	t.Helper()
	u, err := w.users.Create(ctx, users.NewUser{Username: name, Password: "password1", Admin: admin})
	if err != nil {
		t.Fatal(err)
	}
	return users.Principal{UserID: u.ID, Username: u.Username, Admin: admin}
}

func (w *world) team(t *testing.T, slug string) teams.Team {
	t.Helper()
	tm, err := w.teams.Create(ctx, w.admin, teams.Input{Slug: slug, Name: slug})
	if err != nil {
		t.Fatal(err)
	}
	return tm
}

func (w *world) join(t *testing.T, tm teams.Team, p users.Principal, role teams.Role) {
	t.Helper()
	if _, err := w.teams.SetMember(ctx, w.admin, tm.ID, p.UserID, role); err != nil {
		t.Fatal(err)
	}
}

func spec() api.TemplateSpec {
	return api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024}}
}

func ptr(s string) *string { return &s }

// Only administrators make and delete teams, and a slug is a name no user
// has, as a username is a name no team has.
func TestCreate(t *testing.T) {
	w, _ := open(t)
	alice := w.person(t, "alice", false)

	if _, err := w.teams.Create(ctx, alice, teams.Input{Slug: "platform", Name: "Platform"}); !errors.Is(err, teams.ErrForbidden) {
		t.Fatalf("a user made a team: %v", err)
	}
	for _, bad := range []string{"", "Platform", "a--b", "-a", "a.", "a b", "a/b"} {
		if _, err := w.teams.Create(ctx, w.admin, teams.Input{Slug: bad, Name: "x"}); !errors.Is(err, teams.ErrInvalid) {
			t.Errorf("slug %q: %v, want ErrInvalid", bad, err)
		}
	}
	if _, err := w.teams.Create(ctx, w.admin, teams.Input{Slug: "alice", Name: "x"}); !errors.Is(err, teams.ErrConflict) {
		t.Errorf("a team named for a user: %v, want ErrConflict", err)
	}
	tm := w.team(t, "platform")
	if tm.Slug != "platform" || tm.MemberCount != 0 {
		t.Errorf("made %+v", tm)
	}
	if _, err := w.teams.Create(ctx, w.admin, teams.Input{Slug: "platform", Name: "again"}); !errors.Is(err, teams.ErrConflict) {
		t.Errorf("a second team platform: %v, want ErrConflict", err)
	}
	if _, err := w.users.Create(ctx, users.NewUser{Username: "Platform", Password: "password1"}); !errors.Is(err, users.ErrConflict) {
		t.Errorf("a user named for a team: %v, want ErrConflict", err)
	}
	if err := w.teams.Delete(ctx, alice, tm.ID); !errors.Is(err, teams.ErrForbidden) {
		t.Errorf("a user deleted a team: %v", err)
	}
}

// A team's admins run it; its members and viewers do not.
func TestMembers(t *testing.T) {
	w, _ := open(t)
	lead, dev := w.person(t, "lead", false), w.person(t, "dev", false)
	tm := w.team(t, "platform")
	w.join(t, tm, lead, teams.Admin)

	if _, err := w.teams.SetMember(ctx, dev, tm.ID, dev.UserID, teams.Admin); !errors.Is(err, teams.ErrForbidden) {
		t.Fatalf("a non-member made themselves admin: %v", err)
	}
	got, err := w.teams.SetMember(ctx, lead, tm.ID, dev.UserID, teams.Member)
	if err != nil {
		t.Fatal(err)
	}
	if len(got.Members) != 2 || got.Members[0].Username != "lead" || got.Members[1].Role != teams.Member {
		t.Fatalf("members %+v", got.Members)
	}
	if _, err := w.teams.SetMember(ctx, lead, tm.ID, dev.UserID, "owner"); !errors.Is(err, teams.ErrInvalid) {
		t.Errorf("an unknown role: %v", err)
	}
	if _, err := w.teams.Update(ctx, dev, tm.ID, teams.Input{Name: "renamed"}); !errors.Is(err, teams.ErrForbidden) {
		t.Errorf("a member renamed the team: %v", err)
	}
	if got, err := w.teams.Update(ctx, lead, tm.ID, teams.Input{Name: "Platform team"}); err != nil || got.Name != "Platform team" {
		t.Errorf("the admin renamed the team: %+v, %v", got, err)
	}
	if _, err := w.teams.Update(ctx, lead, tm.ID, teams.Input{Slug: "other", Name: "x"}); !errors.Is(err, teams.ErrInvalid) {
		t.Errorf("the slug changed: %v", err)
	}

	list, err := w.teams.List(ctx, dev)
	if err != nil || len(list) != 1 || list[0].Role != teams.Member || list[0].CanManage {
		t.Errorf("the member's list: %+v, %v", list, err)
	}
	if got, err := w.teams.RemoveMember(ctx, lead, tm.ID, dev.UserID); err != nil || len(got.Members) != 1 {
		t.Errorf("removing a member: %+v, %v", got, err)
	}
}

// A team's template is seen and used by its viewers, edited by its members,
// and managed -- visibility, owner, deletion -- by its admins.
func TestTeamTemplates(t *testing.T) {
	w, _ := open(t)
	admin, member, viewer, outsider := w.person(t, "a", false), w.person(t, "m", false), w.person(t, "v", false), w.person(t, "o", false)
	tm := w.team(t, "platform")
	w.join(t, tm, admin, teams.Admin)
	w.join(t, tm, member, teams.Member)
	w.join(t, tm, viewer, teams.Viewer)

	if _, err := w.templates.Create(ctx, viewer, templates.Input{Name: "x", Spec: spec(), Team: &tm.ID}); !errors.Is(err, templates.ErrForbidden) {
		t.Fatalf("a viewer gave the team a template: %v", err)
	}
	if _, err := w.templates.Create(ctx, outsider, templates.Input{Name: "x", Spec: spec(), Team: &tm.ID}); !errors.Is(err, templates.ErrForbidden) {
		t.Fatalf("an outsider gave the team a template: %v", err)
	}
	tpl, err := w.templates.Create(ctx, member, templates.Input{Name: "go", Spec: spec(), Team: &tm.ID})
	if err != nil {
		t.Fatal(err)
	}
	if tpl.Team == nil || tpl.Team.ID != tm.ID || !tpl.CanEdit || tpl.CanManage {
		t.Fatalf("the member's new template: team %+v, edit %v, manage %v", tpl.Team, tpl.CanEdit, tpl.CanManage)
	}

	rights := func(p users.Principal) (bool, bool, bool) {
		got, err := w.templates.Get(ctx, p, tpl.ID)
		if errors.Is(err, templates.ErrNotFound) {
			return false, false, false
		}
		if err != nil {
			t.Fatal(err)
		}
		return true, got.CanEdit, got.CanManage
	}
	for _, c := range []struct {
		who               string
		p                 users.Principal
		see, edit, manage bool
	}{
		{"admin", admin, true, true, true},
		{"member", member, true, true, false},
		{"viewer", viewer, true, false, false},
		{"outsider", outsider, false, false, false},
	} {
		see, edit, manage := rights(c.p)
		if see != c.see || edit != c.edit || manage != c.manage {
			t.Errorf("%s: see %v edit %v manage %v; want %v %v %v", c.who, see, edit, manage, c.see, c.edit, c.manage)
		}
	}

	// The creator is not the owner of a team's template: only the team's
	// admins delete it, and the team cannot go while it owns one.
	if err := w.templates.Delete(ctx, member, tpl.ID); !errors.Is(err, templates.ErrForbidden) {
		t.Errorf("the member deleted the team's template: %v", err)
	}
	if err := w.teams.Delete(ctx, w.admin, tm.ID); !errors.Is(err, teams.ErrConflict) {
		t.Errorf("deleting a team that owns a template: %v, want ErrConflict", err)
	}

	// Taken out of the team, it becomes its taker's own.
	in := templates.Input{Name: tpl.Name, Visibility: tpl.Visibility, Spec: tpl.Spec, Team: ptr("")}
	if _, err := w.templates.Update(ctx, member, tpl.ID, in); !errors.Is(err, templates.ErrForbidden) {
		t.Errorf("the member took the template out of the team: %v", err)
	}
	moved, err := w.templates.Update(ctx, admin, tpl.ID, in)
	if err != nil {
		t.Fatal(err)
	}
	if moved.Team != nil || moved.Owner.ID != admin.UserID {
		t.Errorf("after moving out: team %+v, owner %s", moved.Team, moved.Owner.Username)
	}
	if see, _, _ := rights(viewer); see {
		t.Error("the team's viewer still sees a template the team gave away")
	}
	if err := w.teams.Delete(ctx, w.admin, tm.ID); err != nil {
		t.Errorf("deleting the team once it owns nothing: %v", err)
	}
}

// A collaborator team's viewers use a template, and its members edit it.
func TestTeamCollaborators(t *testing.T) {
	w, _ := open(t)
	owner, member, viewer := w.person(t, "owner", false), w.person(t, "m", false), w.person(t, "v", false)
	tm := w.team(t, "qa")
	w.join(t, tm, member, teams.Member)
	w.join(t, tm, viewer, teams.Viewer)

	tpl, err := w.templates.Create(ctx, owner, templates.Input{Name: "t", Spec: spec()})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := w.templates.Get(ctx, viewer, tpl.ID); !errors.Is(err, templates.ErrNotFound) {
		t.Fatalf("a private template seen before sharing: %v", err)
	}
	tpl, err = w.templates.SetCollaborators(ctx, owner, tpl.ID, nil, []string{tm.ID})
	if err != nil {
		t.Fatal(err)
	}
	if len(tpl.CollaboratorTeams) != 1 || tpl.CollaboratorTeams[0].Slug != "qa" {
		t.Fatalf("collaborator teams %+v", tpl.CollaboratorTeams)
	}
	if got, err := w.templates.Get(ctx, viewer, tpl.ID); err != nil || got.CanEdit {
		t.Errorf("the viewer: %v, edit %v; want to see, not edit", err, got.CanEdit)
	}
	if got, err := w.templates.Get(ctx, member, tpl.ID); err != nil || !got.CanEdit || got.CanManage {
		t.Errorf("the member: %v, edit %v, manage %v; want edit only", err, got.CanEdit, got.CanManage)
	}
	if _, err := w.templates.SetCollaborators(ctx, owner, tpl.ID, nil, []string{"00000000-0000-4000-8000-00000000ffff"}); !errors.Is(err, templates.ErrInvalid) {
		t.Errorf("an unknown team as collaborator: %v", err)
	}
}
