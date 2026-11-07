package profile_test

import (
	"context"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/environments"
)

// What the guest says of its coding agents reaches the server, sent again
// with each session, and the environment's task goes both ways.
func TestAgentsAndTasks(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	em := environments.NewManager(p.d)
	owner := as(p.owner)

	// Reported before there is a session, it is sent when one opens.
	id, _, g := p.env(t, "a", api.Access{})
	g.ReportAgents([]api.AgentSession{{ID: "s1", Agent: api.AgentClaude, State: api.AgentWorking,
		Step: "Edit main.go", Since: time.Now()}})
	eventually(t, "the session's report", func() bool {
		e, err := em.Get(ctx, owner, id)
		return err == nil && len(e.Agents) == 1 && e.Agents[0].Step == "Edit main.go"
	})
	g.ReportAgents(nil)
	eventually(t, "the session to go", func() bool {
		e, err := em.Get(ctx, owner, id)
		return err == nil && len(e.Agents) == 0
	})

	// The agent sets the task, and its owner another, which the guest hears.
	task, err := g.SetTask("Fix rounding")
	if err != nil || task == nil || task.Text != "Fix rounding" || task.SetBy != api.TaskByAgent {
		t.Fatalf("the agent's task: %+v, %v", task, err)
	}
	if _, err := g.SetTask(""); err != nil {
		t.Fatalf("marking it done: %v", err)
	}
	if e, _ := em.Get(ctx, owner, id); e.Task != nil {
		t.Fatalf("after the agent marks it done: %+v", e.Task)
	}
	if _, err := em.SetTask(ctx, owner, id, "Review the audit log"); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the guest to hear the owner's task", func() bool {
		t := g.Task()
		return t != nil && t.Text == "Review the audit log"
	})
	if _, err := g.SetTask(" "); err == nil {
		t.Error("an empty-looking task was taken")
	}
}
