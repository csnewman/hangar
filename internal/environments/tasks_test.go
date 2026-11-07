package environments_test

import (
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
)

// newEnvironment makes a user and an environment of theirs.
func newEnvironment(t *testing.T, d *db.DB, username string) (users.Principal, api.Environment) {
	t.Helper()
	u, err := users.NewManager(d).Create(ctx, users.NewUser{Username: username, Password: "password1"})
	if err != nil {
		t.Fatal(err)
	}
	p := users.Principal{UserID: u.ID, Username: u.Username}
	tpl, err := templates.NewManager(d).Create(ctx, p, templates.Input{Name: "t",
		Spec: api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024, Display: api.DisplayNone}}})
	if err != nil {
		t.Fatal(err)
	}
	env, err := environments.NewManager(d).Create(ctx, p, api.CreateEnvironment{TemplateID: tpl.ID, Name: "e"})
	if err != nil {
		t.Fatal(err)
	}
	return p, env
}

// A task is set by the owner or the agent; setting another ends the one
// before, and both stay in the history.
func TestTasks(t *testing.T) {
	d := dbtest.Open(t)
	em := environments.NewManager(d)
	p, env := newEnvironment(t, d, "owner")
	other, _ := newEnvironment(t, d, "other")

	if env.Task != nil {
		t.Fatalf("a new environment has task %+v", env.Task)
	}
	first, err := em.SetTask(ctx, p, env.ID, "  Fix rounding\nin checkout totals ")
	if err != nil {
		t.Fatal(err)
	}
	if first.Text != "Fix rounding in checkout totals" || first.SetBy != api.TaskByUser {
		t.Fatalf("set: %+v", first)
	}
	again, err := em.SetTask(ctx, p, env.ID, "Fix rounding in checkout totals")
	if err != nil || again.ID != first.ID {
		t.Fatalf("the same text again: %+v, %v; want task %s", again, err, first.ID)
	}
	second, err := em.AgentSetTask(ctx, env.ID, "Add an audit record to postings")
	if err != nil || second == nil || second.SetBy != api.TaskByAgent {
		t.Fatalf("the agent's task: %+v, %v", second, err)
	}
	got, err := em.Get(ctx, p, env.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.Task == nil || got.Task.ID != second.ID || got.Task.Text != second.Text {
		t.Fatalf("the environment's task: %+v, want %+v", got.Task, second)
	}
	if cur, err := em.Task(ctx, env.ID); err != nil || cur == nil || cur.ID != second.ID {
		t.Fatalf("Task: %+v, %v", cur, err)
	}

	history, err := em.Tasks(ctx, p, env.ID)
	if err != nil {
		t.Fatal(err)
	}
	if len(history) != 2 || history[0].ID != second.ID || history[0].DoneAt != nil ||
		history[1].ID != first.ID || history[1].DoneAt == nil {
		t.Fatalf("history: %+v", history)
	}

	if err := em.EndTask(ctx, p, env.ID); err != nil {
		t.Fatal(err)
	}
	if got, _ := em.Get(ctx, p, env.ID); got.Task != nil {
		t.Fatalf("after it is done: %+v", got.Task)
	}
	if cur, err := em.Task(ctx, env.ID); err != nil || cur != nil {
		t.Fatalf("Task after it is done: %+v, %v", cur, err)
	}

	for _, text := range []string{"", " \n ", strings.Repeat("x", api.MaxTaskLength+1)} {
		if _, err := em.SetTask(ctx, p, env.ID, text); !errors.Is(err, environments.ErrInvalid) {
			t.Errorf("task %.20q: %v, want ErrInvalid", text, err)
		}
	}
	if _, err := em.SetTask(ctx, other, env.ID, "mine now"); !errors.Is(err, environments.ErrNotFound) {
		t.Errorf("another user's set: %v, want ErrNotFound", err)
	}
	if _, err := em.Tasks(ctx, other, env.ID); !errors.Is(err, environments.ErrNotFound) {
		t.Errorf("another user's history: %v, want ErrNotFound", err)
	}
}

// The agents' sessions are what the guest last reported; a finished one
// stops needing its owner once they look, and goes idle.
func TestAgentSessions(t *testing.T) {
	d := dbtest.Open(t)
	em := environments.NewManager(d)
	p, env := newEnvironment(t, d, "owner")

	since := time.Now().Add(-time.Minute).UTC().Truncate(time.Millisecond)
	report := []api.AgentSession{
		{ID: "a", Agent: api.AgentClaude, Terminal: "t1", State: api.AgentFinished, Since: since},
		{ID: "b", Agent: api.AgentCodex, State: api.AgentWorking, Step: "Edit main.go", Since: since.Add(time.Second)},
	}
	if err := em.ReportAgents(ctx, env.ID, report); err != nil {
		t.Fatal(err)
	}
	got, err := em.Get(ctx, p, env.ID)
	if err != nil {
		t.Fatal(err)
	}
	if len(got.Agents) != 2 || got.Agents[0].ID != "a" || !got.Agents[0].NeedsYou() || got.Agents[0].Terminal != "t1" ||
		!got.Agents[0].Since.Equal(since) || got.Agents[1].Step != "Edit main.go" || got.Agents[1].NeedsYou() {
		t.Fatalf("agents: %+v", got.Agents)
	}

	if err := em.SeenAgent(ctx, p, env.ID, "a"); err != nil {
		t.Fatal(err)
	}
	if err := em.SeenAgent(ctx, p, env.ID, "nope"); !errors.Is(err, environments.ErrNotFound) {
		t.Errorf("seeing a session that is not there: %v", err)
	}
	got, _ = em.Get(ctx, p, env.ID)
	if a := got.Agents[0]; a.State != api.AgentIdle || !a.Seen || a.NeedsYou() {
		t.Fatalf("after it is seen: %+v", a)
	}

	// It asks later: seen before is not seen now. b has gone.
	report = []api.AgentSession{{ID: "a", Agent: api.AgentClaude, State: api.AgentAsking,
		Message: "Claude needs your permission to use Bash", Since: time.Now().Add(time.Second)}}
	if err := em.ReportAgents(ctx, env.ID, report); err != nil {
		t.Fatal(err)
	}
	got, _ = em.Get(ctx, p, env.ID)
	if len(got.Agents) != 1 || got.Agents[0].State != api.AgentAsking || !got.Agents[0].NeedsYou() {
		t.Fatalf("after it asks: %+v", got.Agents)
	}
}

// Every change a client shows is announced on Channel, and usage figures
// are not.
func TestChangesAreAnnounced(t *testing.T) {
	d := dbtest.Open(t)
	em := environments.NewManager(d)
	p, env := newEnvironment(t, d, "owner")

	heard := make(chan string, 16)
	ready := make(chan struct{})
	go d.Listen(t.Context(), func(channel, payload string) {
		if channel == "" {
			close(ready)
			return
		}
		heard <- payload
	}, environments.Channel)
	<-ready

	expect := func(what string) {
		t.Helper()
		select {
		case id := <-heard:
			if id != env.ID {
				t.Fatalf("%s: heard %q, want %q", what, id, env.ID)
			}
		case <-time.After(5 * time.Second):
			t.Fatalf("%s: nothing heard", what)
		}
	}
	quiet := func(what string) {
		t.Helper()
		select {
		case id := <-heard:
			t.Fatalf("%s: heard %q", what, id)
		case <-time.After(300 * time.Millisecond):
		}
	}

	if _, err := em.SetTask(ctx, p, env.ID, "a task"); err != nil {
		t.Fatal(err)
	}
	expect("a task set")
	if err := em.ReportAgents(ctx, env.ID, []api.AgentSession{{ID: "a", Agent: api.AgentClaude,
		State: api.AgentWorking, Since: time.Now()}}); err != nil {
		t.Fatal(err)
	}
	expect("a session reported")

	err := d.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `UPDATE environments SET stats = '{"cpu": 1}' WHERE id = $1`, env.ID)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	quiet("usage figures")

	if _, err := em.SetDesired(ctx, p, env.ID, api.DesiredStopped); err != nil {
		t.Fatal(err)
	}
	expect("a desired state")
}
